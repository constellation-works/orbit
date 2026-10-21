//! Block episodes, their sources and recorded final-recovery decisions, and what the backstop does about each.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use orbit_engine::{WORKFLOW_RUN_FAILED_EVENT, WORKFLOW_RUN_INTERRUPTED_EVENT};
use orbit_types::task::{
    ArtifactManifestFileV2, Task, TaskComment, TaskDecision, TaskHistoryEntry, TaskStatus,
};

use super::super::helpers::{SYSTEM_ACTOR_LABEL, is_automation_actor};
use super::super::legacy_decisions;
use super::backstop::note_field;
use super::{ATTRIBUTION_SLACK_MS, MAX_EPISODE_AGE_HOURS};

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
        // [ORB-14269] An implementer-declared blocker is the same shape of
        // stop: another agent does not clear it. The marker is what a
        // `workflow_run_failed` fallback note still carries.
        // A claimed leaf records its failure diagnostic in the task's
        // execution summary; its `claim_failed` history entry may have no
        // status note. Check that durable evidence too, or the backstop would
        // spend a recovery agent immediately after the implementer stopped.
        let agent_blocker = orbit_types::workflow::is_task_blocked_by_agent(None, Some(&note))
            || (source == BlockSource::ClaimFailed
                && orbit_types::workflow::is_task_blocked_by_agent(
                    None,
                    Some(&task.execution_summary),
                ));
        if orbit_types::workflow::is_validation_environment_failure(None, Some(&note))
            || agent_blocker
        {
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
    /// When the decision was recorded.
    pub at: DateTime<Utc>,
    /// The run the decision belongs to.
    pub run_id: String,
    /// The decision kind the agent proposed.
    pub decision: String,
    /// What the applier did with it.
    pub outcome: String,
}

impl FinalRecoveryRecord {
    /// The newest decision recorded on a task with `history` and `comments`.
    pub fn last(history: &[TaskHistoryEntry], comments: &[TaskComment]) -> Option<Self> {
        Self::newest_since(history, comments, DateTime::<Utc>::MIN_UTC)
    }

    /// The newest decision recorded at or after `since`: the applier's typed
    /// decisions, and a legacy decision comment for a run none of them names.
    pub(super) fn newest_since(
        history: &[TaskHistoryEntry],
        comments: &[TaskComment],
        since: DateTime<Utc>,
    ) -> Option<Self> {
        let typed = history.iter().filter_map(Self::typed).collect::<Vec<_>>();
        let legacy = comments
            .iter()
            .filter_map(legacy_decisions::final_recovery_comment)
            .filter(|legacy| !typed.iter().any(|typed| typed.run_id == legacy.run_id))
            .collect::<Vec<_>>();
        typed
            .into_iter()
            .chain(legacy)
            .filter(|record| record.at >= since)
            .max_by_key(|record| record.at)
    }

    fn typed(entry: &TaskHistoryEntry) -> Option<Self> {
        let Some(TaskDecision::FinalRecovery {
            run_id,
            decision,
            outcome,
        }) = &entry.decision
        else {
            return None;
        };
        (entry.by == SYSTEM_ACTOR_LABEL).then(|| Self {
            at: entry.at,
            run_id: run_id.clone(),
            decision: decision.clone(),
            outcome: outcome.as_str().to_string(),
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
    /// The task was written after the block with no recorded actor (for
    /// example, a legacy field-only write), so a human's edit cannot be ruled out.
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
    if let Some(record) = FinalRecoveryRecord::newest_since(history, comments, episode.blocked_at) {
        return EpisodeDisposition::Decided(record);
    }
    match later_touch(episode.blocked_at, updated_at, history, comments, artifacts) {
        Some(LaterTouch::Human { by }) => return EpisodeDisposition::HumanIntervened { by },
        Some(LaterTouch::Unexplained { changed_at }) => {
            return EpisodeDisposition::UnexplainedChange { changed_at };
        }
        None => {}
    }
    if now - episode.blocked_at > Duration::hours(MAX_EPISODE_AGE_HOURS) {
        return EpisodeDisposition::TooOld;
    }
    EpisodeDisposition::Eligible
}

/// A write to a task after its block that Orbit's automation did not make,
/// or cannot be shown to have made.
pub(super) enum LaterTouch {
    /// Someone outside Orbit's automation commented, changed a field or the
    /// status, or attached an artifact.
    Human { by: String },
    /// The task changed with no recorded actor.
    Unexplained { changed_at: DateTime<Utc> },
}

/// Whether anyone but Orbit's automation touched the task after `since`:
/// the human-intervention rule the backstop and the blocked-task triage
/// share. `updated_at` is the task's last write.
pub(super) fn later_touch(
    since: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    history: &[TaskHistoryEntry],
    comments: &[TaskComment],
    artifacts: &[ArtifactManifestFileV2],
) -> Option<LaterTouch> {
    let later_actor = history
        .iter()
        .filter(|entry| entry.at > since)
        .map(|entry| entry.by.as_str())
        .chain(
            comments
                .iter()
                .filter(|comment| comment.at > since)
                .map(|comment| comment.by.as_str()),
        )
        .chain(
            artifacts
                .iter()
                .filter(|artifact| artifact.created_at > since)
                .map(|artifact| artifact.created_by.as_str()),
        )
        .find(|by| !is_automation_actor(by));
    if let Some(by) = later_actor {
        return Some(LaterTouch::Human { by: by.to_string() });
    }
    // Every attributed write above is automation's. A newer write than all of
    // them changed a field without recording who did it: fail closed.
    let last_attributed = history
        .iter()
        .map(|entry| entry.at)
        .chain(comments.iter().map(|comment| comment.at))
        .chain(artifacts.iter().map(|artifact| artifact.created_at))
        .filter(|at| *at >= since)
        .max()
        .unwrap_or(since);
    (updated_at - last_attributed > Duration::milliseconds(ATTRIBUTION_SLACK_MS)).then_some(
        LaterTouch::Unexplained {
            changed_at: updated_at,
        },
    )
}
