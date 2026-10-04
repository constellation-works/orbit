//! The blocked-task recovery backstop.
//!
//! Some failures block a task where no pipeline hook sees them: run
//! finalization (`workflow_run_failed`, `workflow_run_interrupted`), a failed
//! claim settlement (`claim_failed`), and a gate or auto run that fails before
//! its leaf starts (a `workflow_run_failed` naming `task_gate_pipeline` or
//! `task_auto_pipeline`). On the owner, each clock tick finds those blocks and
//! dispatches `blocked_task_recovery_pipeline` for each, which runs the
//! `final_recovery` activity and applies its decision through
//! [`OrbitRuntime::apply_final_recovery`] — the same applier, crew pool and
//! requeue bound as every other final-recovery caller.
//!
//! A *block episode* is the history entry that moved the task to `blocked`.
//! Each episode gets at most one recovery, and none when:
//!
//! - a final-recovery decision was already recorded on or after the block;
//! - someone other than Orbit's own automation commented on the task,
//!   changed its status or attached an artifact after the block — a human's
//!   call stands;
//! - the task was written after the block by a change no actor is recorded
//!   for — a field-only edit leaves no history, so the backstop cannot prove
//!   it was not a human's and leaves the task alone;
//! - a recovery run for the episode already exists (the episode key is also
//!   the run's admission key, so two ticks cannot admit two runs);
//! - the block is older than [`MAX_EPISODE_AGE_HOURS`];
//! - the blocking note carries the validation-environment marker: required
//!   validation lacked a tool, which no task decision fixes [ORB-13987].
//!
//! The backstop never resumes: `resume` is escalated, and `requeue` is how
//! work restarts. A recovery run that ends without applying a decision — the
//! agent crashed, timed out or the worker died — is settled on a later tick as
//! an escalation, so every dispatched episode ends with one recorded decision.
//!
//! Enabled when `workflow.final_recovery_crews` is non-empty and this runtime
//! owns the workspace's task records; `final_recovery_crews = []` turns it off.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::git::run_git;
use orbit_engine::{WORKFLOW_RUN_FAILED_EVENT, WORKFLOW_RUN_INTERRUPTED_EVENT};
use orbit_store::contracts::JobRunQuery;
use orbit_types::identity::{MACHINE_ID_PREFIX, agent_from_model, all_agent_families};
use orbit_types::task::{ArtifactManifestFileV2, Task, TaskComment, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{JobRun, JobRunState, JobRunTrigger};
use serde_json::{Value, json};

use super::final_recovery::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest, FinalRecoveryRequeueBound,
    FinalRecoveryTaskRevision,
};
use super::helpers::SYSTEM_ACTOR_LABEL;
use crate::OrbitRuntime;
use crate::application::job::pipeline::{
    PipelineSubmission, ROUTINE_DISPATCH_ORBIT_DIR_FIELD, RetryKey,
};

/// The job the backstop dispatches, one run per block episode.
pub const BLOCKED_TASK_RECOVERY_JOB: &str = "blocked_task_recovery_pipeline";
/// Recovery runs live at once; further episodes wait for a later tick.
pub const MAX_ACTIVE_BLOCKED_RECOVERIES: usize = 2;
/// Blocks older than this are left to a human. It also bounds the first tick
/// after an upgrade, which would otherwise recover every historical block.
pub const MAX_EPISODE_AGE_HOURS: i64 = 72;
/// How far a task's `updated_at` may trail its newest attributed write and
/// still be that write: an appended comment or history entry is stamped just
/// before the record is rewritten.
pub(crate) const ATTRIBUTION_SLACK_MS: i64 = 500;
/// Run-input field holding the episode key; the keyed admission matches it.
const EPISODE_KEY_FIELD: &str = "episode_key";
/// Newest recovery runs the keyed admission and the tick look through.
const RUN_SCAN_LIMIT: usize = 500;
/// Prefix of the comment header the final-recovery applier writes.
const DECISION_COMMENT_PREFIX: &str = "final_recovery run_id=";
const TRIGGER_NAME: &str = "blocked-task-recovery";
const TRIGGER_CONSUMER: &str = "clock-sweep";

/// What put a task into its current block, for the blocks the backstop owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockSource {
    /// A delivery run failed, timed out or was cancelled.
    RunFailed,
    /// A run was reconciled `interrupted` (its worker died).
    RunInterrupted,
    /// A gate or auto run failed before or around its leaf.
    GateFailed,
    /// A claimed leaf's settlement reported failure to the owner.
    ClaimFailed,
}

impl BlockSource {
    /// Stable name recorded in the run input.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunFailed => "run_failed",
            Self::RunInterrupted => "run_interrupted",
            Self::GateFailed => "gate_failed",
            Self::ClaimFailed => "claim_failed",
        }
    }

    fn of(entry: &TaskHistoryEntry) -> Option<Self> {
        match entry.event.as_str() {
            WORKFLOW_RUN_FAILED_EVENT => {
                let job = entry
                    .note
                    .as_deref()
                    .and_then(|note| note_field(note, "job="));
                Some(match job {
                    Some("task_gate_pipeline" | "task_auto_pipeline") => Self::GateFailed,
                    _ => Self::RunFailed,
                })
            }
            WORKFLOW_RUN_INTERRUPTED_EVENT => Some(Self::RunInterrupted),
            "claim_failed" => Some(Self::ClaimFailed),
            _ => None,
        }
    }
}

/// The task's current block, when one of [`BlockSource`] wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockEpisode {
    /// The blocked task.
    pub task_id: String,
    /// What blocked it.
    pub source: BlockSource,
    /// When the blocking history entry was recorded; identifies the episode.
    pub blocked_at: DateTime<Utc>,
    /// The run that failed, when one is known.
    pub failed_run_id: Option<String>,
    /// The blocking entry's note.
    pub note: String,
}

impl BlockEpisode {
    /// The current episode of `task`, from its history.
    pub fn current(task: &Task, history: &[TaskHistoryEntry]) -> Option<Self> {
        if task.status != TaskStatus::Blocked {
            return None;
        }
        let entry = history
            .iter()
            .rev()
            .find(|entry| entry.to_status == Some(TaskStatus::Blocked))?;
        let source = BlockSource::of(entry)?;
        let note = entry.note.clone().unwrap_or_default();
        // [ORB-13987] Required validation lacked a tool: the host needs
        // fixing, not the task, so no recovery agent is spent on it.
        if orbit_types::workflow::is_validation_environment_failure(None, Some(&note)) {
            return None;
        }
        let failed_run_id = note_field(&note, "run_id=")
            .map(str::to_string)
            .or_else(|| task.job_run_id.clone());
        Some(Self {
            task_id: task.id.clone(),
            source,
            blocked_at: entry.at,
            failed_run_id,
            note,
        })
    }

    /// The key one recovery run is admitted under.
    pub fn key(&self) -> String {
        format!(
            "{}@{}",
            self.task_id,
            self.blocked_at.to_rfc3339_opts(SecondsFormat::Nanos, true)
        )
    }
}

/// The last final-recovery decision recorded on a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalRecoveryRecord {
    /// When the decision comment was written.
    pub at: DateTime<Utc>,
    /// The run the decision belongs to.
    pub run_id: String,
    /// The decision kind the agent proposed.
    pub decision: String,
    /// What the applier did with it.
    pub outcome: String,
}

impl FinalRecoveryRecord {
    /// The newest decision comment among `comments`.
    pub fn last(comments: &[TaskComment]) -> Option<Self> {
        comments.iter().rev().find_map(Self::parse)
    }

    fn parse(comment: &TaskComment) -> Option<Self> {
        if comment.by != SYSTEM_ACTOR_LABEL {
            return None;
        }
        let header = comment.message.lines().next()?;
        if !header.starts_with(DECISION_COMMENT_PREFIX) {
            return None;
        }
        Some(Self {
            at: comment.at,
            run_id: note_field(header, "run_id=")?.to_string(),
            decision: note_field(header, "decision=")?.to_string(),
            outcome: note_field(header, "outcome=").unwrap_or("-").to_string(),
        })
    }
}

/// Why an episode is or is not recovered now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EpisodeDisposition {
    /// No recovery yet; the backstop will dispatch one.
    Eligible,
    /// A final-recovery decision is already recorded for this episode.
    Decided(FinalRecoveryRecord),
    /// Someone outside Orbit's automation acted after the block.
    HumanIntervened {
        /// Who acted.
        by: String,
    },
    /// The task was written after the block with no recorded actor (a
    /// field-only edit), so a human's edit cannot be ruled out.
    UnexplainedChange {
        /// The unattributed write.
        changed_at: DateTime<Utc>,
    },
    /// Blocked longer than [`MAX_EPISODE_AGE_HOURS`].
    TooOld,
}

/// Classify `episode` from the task's last write (`updated_at`) and the
/// attributed writes recorded for it: history, comments and artifacts.
pub fn episode_disposition(
    episode: &BlockEpisode,
    updated_at: DateTime<Utc>,
    history: &[TaskHistoryEntry],
    comments: &[TaskComment],
    artifacts: &[ArtifactManifestFileV2],
    now: DateTime<Utc>,
) -> EpisodeDisposition {
    if let Some(record) = comments
        .iter()
        .rev()
        .filter(|comment| comment.at >= episode.blocked_at)
        .find_map(FinalRecoveryRecord::parse)
    {
        return EpisodeDisposition::Decided(record);
    }
    let later_actor = history
        .iter()
        .filter(|entry| entry.at > episode.blocked_at)
        .map(|entry| entry.by.as_str())
        .chain(
            comments
                .iter()
                .filter(|comment| comment.at > episode.blocked_at)
                .map(|comment| comment.by.as_str()),
        )
        .chain(
            artifacts
                .iter()
                .filter(|artifact| artifact.created_at > episode.blocked_at)
                .map(|artifact| artifact.created_by.as_str()),
        )
        .find(|by| !is_automation_actor(by));
    if let Some(by) = later_actor {
        return EpisodeDisposition::HumanIntervened { by: by.to_string() };
    }
    // Every attributed write above is automation's. A newer write than all of
    // them changed a field without recording who did it: fail closed.
    let last_attributed = history
        .iter()
        .map(|entry| entry.at)
        .chain(comments.iter().map(|comment| comment.at))
        .chain(artifacts.iter().map(|artifact| artifact.created_at))
        .filter(|at| *at >= episode.blocked_at)
        .max()
        .unwrap_or(episode.blocked_at);
    if updated_at - last_attributed > Duration::milliseconds(ATTRIBUTION_SLACK_MS) {
        return EpisodeDisposition::UnexplainedChange {
            changed_at: updated_at,
        };
    }
    if now - episode.blocked_at > Duration::hours(MAX_EPISODE_AGE_HOURS) {
        return EpisodeDisposition::TooOld;
    }
    EpisodeDisposition::Eligible
}

/// Whether `label` is one of Orbit's own writers: the system, a machine (claim
/// settlement), the task pilot, or an agent. Any other label — `human:<user>`,
/// `operator`, an `ORBIT_ACTOR` or dashboard author — counts as a human.
fn is_automation_actor(label: &str) -> bool {
    let label = label.trim();
    label == SYSTEM_ACTOR_LABEL
        || label == "task-pilot"
        || label.starts_with(MACHINE_ID_PREFIX)
        || all_agent_families().contains(&label)
        || agent_from_model(label).is_some()
}

/// The run input the backstop dispatches with; also how a recovery run finds
/// its episode again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedRecoveryInput {
    /// The blocked task.
    pub task_id: String,
    /// [`BlockEpisode::key`] of the episode being recovered.
    pub episode_key: String,
    /// [`BlockSource::as_str`].
    pub block_source: String,
    /// The run that failed, when known.
    pub failed_run_id: Option<String>,
    /// The task revision observed when the recovery was dispatched.
    pub observed: FinalRecoveryTaskRevision,
}

impl BlockedRecoveryInput {
    /// The run-input JSON.
    pub fn to_json(&self) -> Value {
        json!({
            "task_id": self.task_id,
            EPISODE_KEY_FIELD: self.episode_key,
            "block_source": self.block_source,
            "failed_run_id": self.failed_run_id.clone().unwrap_or_default(),
            "observed_status": self.observed.status.to_string(),
            "observed_updated_at": self
                .observed
                .updated_at
                .to_rfc3339_opts(SecondsFormat::Nanos, true),
        })
    }

    /// Read a run input written by [`Self::to_json`].
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let text = |field: &str| -> Result<String, String> {
            value
                .get(field)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .ok_or_else(|| format!("blocked-task recovery input lacks `{field}`"))
        };
        let status = text("observed_status")?
            .parse::<TaskStatus>()
            .map_err(|error| format!("observed_status: {error}"))?;
        let updated_at = DateTime::parse_from_rfc3339(&text("observed_updated_at")?)
            .map_err(|error| format!("observed_updated_at: {error}"))?
            .with_timezone(&Utc);
        Ok(Self {
            task_id: text("task_id")?,
            episode_key: text(EPISODE_KEY_FIELD)?,
            block_source: text("block_source")?,
            failed_run_id: text("failed_run_id").ok(),
            observed: FinalRecoveryTaskRevision { status, updated_at },
        })
    }
}

/// What one backstop tick did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockedRecoveryTick {
    /// Why the tick did nothing, when it stood down.
    pub skipped: Option<String>,
    /// `(task_id, run_id)` for each recovery dispatched.
    pub dispatched: Vec<(String, String)>,
    /// Tasks whose recovery run ended without a decision and were escalated.
    pub settled: Vec<String>,
}

/// One blocked task as the backstop sees it, for `orbit doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedRecoveryView {
    /// The blocked episode.
    pub episode: BlockEpisode,
    /// What the backstop does about it.
    pub disposition: EpisodeDisposition,
}

/// The checkout a recovery run's agent inspects: a detached linked worktree of
/// the base, private to the run and removed when its decision is applied.
pub(crate) fn recovery_checkout_path(
    state_dir: &Path,
    run_id: &str,
) -> Result<PathBuf, OrbitError> {
    if run_id.is_empty()
        || !run_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(OrbitError::InvalidInput(format!(
            "recovery run id '{run_id}' cannot name a checkout"
        )));
    }
    Ok(state_dir.join("recovery-checkouts").join(run_id))
}

/// What a recovery run's first step established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BlockedRecoveryPreparation {
    /// The episode is no longer the task's untouched block; do nothing.
    Skip {
        /// Why.
        reason: String,
    },
    /// The agent's input.
    Ready(PreparedBlockedRecovery),
}

/// The final-recovery agent's view of a blocked episode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedBlockedRecovery {
    /// The run's private detached checkout of `base_sha`.
    pub(crate) checkout: PathBuf,
    /// Base ref a `complete_no_diff` commit must be reachable from.
    pub(crate) base_ref: String,
    /// The commit `base_ref` resolved to.
    pub(crate) base_sha: String,
    /// The run that failed, or the recovery run when none is known.
    pub(crate) failed_run_id: String,
    /// The failed step, or the block source when no run step is known.
    pub(crate) failed_step_id: String,
    /// The failed run's job, when known.
    pub(crate) job_id: Option<String>,
    /// The failure as recorded.
    pub(crate) error_message: String,
}

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
            return Some("`workflow.final_recovery_crews` is empty".to_string());
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
        let orbit_dir = self.shared_root();
        self.run_blocked_task_recovery_tick_with(now, &mut |input| {
            let mut input = input;
            input[ROUTINE_DISPATCH_ORBIT_DIR_FIELD] = json!(orbit_dir.to_string_lossy());
            let submission = PipelineSubmission {
                retry_key: Some(RetryKey {
                    field: EPISODE_KEY_FIELD,
                    scan_limit: RUN_SCAN_LIMIT,
                }),
                trigger: JobRunTrigger::state_routine(TRIGGER_NAME, TRIGGER_CONSUMER),
                ..PipelineSubmission::catalog(
                    BLOCKED_TASK_RECOVERY_JOB,
                    input,
                    Some(SYSTEM_ACTOR_LABEL),
                )
            };
            self.submit_keyed_pipeline_run(submission)
                .map(|(result, _)| result.run_id)
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
                observed: FinalRecoveryTaskRevision::of(&task),
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
        if FinalRecoveryTaskRevision::of(&task) != input.observed {
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
        let failed_run = match &episode.failed_run_id {
            Some(run_id) => self.get_job_run_backend(run_id)?,
            None => None,
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
            Some(self.get_task(&input.task_id)?.execution_summary)
        } else {
            None
        };
        let (base_ref, base_sha) = self.recovery_base()?;
        let checkout = self.create_recovery_checkout(recovery_run_id, &base_sha)?;
        Ok(BlockedRecoveryPreparation::Ready(PreparedBlockedRecovery {
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
            error_message: failed_step
                .and_then(|step| step.error_message.clone())
                .filter(|message| !message.trim().is_empty())
                .or_else(|| settled_failure.filter(|message| !message.trim().is_empty()))
                .unwrap_or_else(|| episode.note.clone()),
        }))
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
    /// returned none. `resume` is escalated: this backstop restarts work only
    /// by `requeue`. The decision comment names `recovery_run_id`. A verified
    /// `complete_no_diff` moves the task to `review`, never `done`.
    pub(crate) fn apply_blocked_task_recovery(
        &self,
        input: &BlockedRecoveryInput,
        recovery_run_id: &str,
        base_ref: &str,
        output: Option<&Value>,
    ) -> Result<FinalRecoveryOutcome, OrbitError> {
        let output = output.map(|output| without_resume(output, input));
        let request = FinalRecoveryRequest {
            task_id: input.task_id.clone(),
            run_id: recovery_run_id.to_string(),
            observed: input.observed,
            repo_root: self.paths().repo_root.clone(),
            base_ref: base_ref.to_string(),
            completion: FinalRecoveryCompletion::Review,
            requeue_bound: FinalRecoveryRequeueBound::default(),
        };
        self.apply_final_recovery(&request, output.as_ref())
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
            std::fs::create_dir_all(parent).map_err(|error| {
                OrbitError::Execution(format!(
                    "create recovery checkout directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let target = path.to_string_lossy().to_string();
        let output = run_git(
            &self.paths().repo_root,
            &["worktree", "add", "--detach", "--quiet", &target, base_sha],
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
        std::fs::create_dir(&denied_root).map_err(|error| {
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
        let output = run_git(
            &self.paths().repo_root,
            &["worktree", "remove", "--force", &target],
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
        let comments = self.get_task_comments(&input.task_id)?;
        if comments
            .iter()
            .filter(|comment| comment.at >= episode.blocked_at)
            .any(|comment| FinalRecoveryRecord::parse(comment).is_some())
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

/// `output` with `resume` replaced by an escalation naming the failed run.
fn without_resume(output: &Value, input: &BlockedRecoveryInput) -> Value {
    if output.get("decision").and_then(Value::as_str) != Some("resume") {
        return output.clone();
    }
    let step = output.get("step_id").and_then(Value::as_str).unwrap_or("-");
    let rationale = output
        .get("rationale")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let human_action = match &input.failed_run_id {
        Some(run_id) => format!(
            "Resume `{run_id}` from `{step}` with `orbit job resume {run_id}` if the diagnosis \
             holds, or requeue the task."
        ),
        None => "Requeue the task once the diagnosis is addressed.".to_string(),
    };
    json!({
        "decision": "escalate",
        "diagnosis": format!(
            "final recovery proposed resuming from `{step}`, which the blocked-task backstop \
             never does: {rationale}"
        ),
        "human_action": human_action,
    })
}

/// The `key=value` token in a history note or comment header.
fn note_field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
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
