//! Which block episodes await a human, kept for the history it reads.
//!
//! Final recovery is the single decider for blocked tasks: it settles what
//! it can mechanically and otherwise opens one decision request on the task
//! (see `final_recovery`). The retired blocked-task triage left
//! [`BLOCKED_TASK_TRIAGED_EVENT`] markers and `block_triage` requests
//! whose options live in a proposal comment; readers keep parsing both.
//! [`triage_candidacy`] stays as the one rule the auto-task
//! `skip_unless_blocked_candidates` precondition reads: a block episode
//! qualifies while final recovery's escalation, or the backstop's silence,
//! leaves it for a human.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{ArtifactManifestFileV2, Task, TaskComment, TaskHistoryEntry, TaskStatus};
use serde::Serialize;

use super::super::helpers::{SYSTEM_ACTOR_LABEL, is_automation_actor};
use super::MAX_EPISODE_AGE_HOURS;
use super::episode::{FinalRecoveryRecord, LaterTouch, later_touch};
use super::readout::{blocking_run_id, current_block, is_escalation};
use crate::OrbitRuntime;

/// History event the retired blocked-task triage appended to mark an episode
/// triaged. Nothing writes it any more; old markers still count.
pub const BLOCKED_TASK_TRIAGED_EVENT: &str = "blocked_task_triaged";

/// History event the retired blocked-task triage appended when it resumed a
/// run. The requeue bound still counts old ones.
pub const BLOCKED_TASK_TRIAGE_RESUMED_EVENT: &str = "blocked_task_triage_resumed";

/// Why a block episode qualifies for triage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriageBasis {
    /// Its recorded final-recovery decision is an escalation.
    Escalated,
    /// It has no decision, and the backstop will never make one.
    Undecided,
}

/// A block episode the triage reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TriageEpisode {
    /// The blocked task.
    pub task_id: String,
    /// When the task last moved to `blocked`; identifies the episode.
    pub blocked_at: DateTime<Utc>,
    /// Why it qualifies.
    pub basis: TriageBasis,
    /// The run the block names, when one is known.
    pub run_id: Option<String>,
    /// The note of the entry that explains the block.
    pub block_note: String,
    /// The escalation's recorded comment, when it wrote one.
    pub escalation: Option<String>,
}

impl TriageEpisode {
    /// The episode's identity, as its triage marker records it: the time the
    /// task last moved to `blocked`, to the nanosecond.
    pub fn key(&self) -> String {
        episode_key(self.blocked_at)
    }
}

fn episode_key(blocked_at: DateTime<Utc>) -> String {
    blocked_at.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

/// Whether a task's current block episode awaits triage, and why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriageCandidacy {
    /// The episode awaits triage.
    Qualifies(TriageEpisode),
    /// The task is not `blocked`.
    NotBlocked,
    /// The task is blocked but no history entry records the block.
    NoEpisode,
    /// The triage already recorded a decision for this episode.
    Triaged {
        /// When it did.
        at: DateTime<Utc>,
    },
    /// Someone outside Orbit's automation acted after the block.
    HumanIntervened {
        /// Who acted.
        by: String,
    },
    /// The task changed after the block with no recorded actor.
    UnexplainedChange {
        /// The unattributed write.
        changed_at: DateTime<Utc>,
    },
    /// The episode's recorded decision is not an escalation.
    NotEscalated {
        /// The recorded outcome.
        outcome: String,
    },
    /// No decision yet, and the backstop may still make one.
    AwaitingBackstop,
}

impl TriageCandidacy {
    /// Stable token for the candidacy.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Qualifies(_) => "qualifies",
            Self::NotBlocked => "not_blocked",
            Self::NoEpisode => "no_episode",
            Self::Triaged { .. } => "triaged",
            Self::HumanIntervened { .. } => "human_intervened",
            Self::UnexplainedChange { .. } => "unexplained_change",
            Self::NotEscalated { .. } => "not_escalated",
            Self::AwaitingBackstop => "awaiting_backstop",
        }
    }

    /// Human-readable detail for the candidacy, naming the episode state
    /// behind it.
    pub fn detail(&self) -> String {
        match self {
            Self::Qualifies(episode) => format!("qualifies ({:?})", episode.basis),
            Self::NotBlocked => "the task is not blocked".to_string(),
            Self::NoEpisode => "no history entry records the block".to_string(),
            Self::Triaged { at } => format!("already triaged at {}", at.to_rfc3339()),
            Self::HumanIntervened { by } => format!("{by} acted after the block"),
            Self::UnexplainedChange { changed_at } => format!(
                "the task changed at {} with no recorded actor",
                changed_at.to_rfc3339()
            ),
            Self::NotEscalated { outcome } => {
                format!("its final-recovery decision is `{outcome}`, not an escalation")
            }
            Self::AwaitingBackstop => {
                "no decision yet; the blocked-task backstop may still recover it".to_string()
            }
        }
    }
}

/// Classify `task`'s current block episode for triage at `now`.
///
/// An episode qualifies when its newest recorded final-recovery decision is
/// an escalation, or when it has none and is older than the backstop's
/// [`MAX_EPISODE_AGE_HOURS`]. It never qualifies once triaged, when a human
/// set the block, nor after a human write — the backstop's rule.
pub fn triage_candidacy(
    task: &Task,
    history: &[TaskHistoryEntry],
    comments: &[TaskComment],
    artifacts: &[ArtifactManifestFileV2],
    now: DateTime<Utc>,
) -> TriageCandidacy {
    if task.status != TaskStatus::Blocked {
        return TriageCandidacy::NotBlocked;
    }
    let Some(start) = history
        .iter()
        .rposition(|entry| entry.to_status == Some(TaskStatus::Blocked))
    else {
        return TriageCandidacy::NoEpisode;
    };
    let blocked_at = history[start].at;
    let episode = &history[start..];
    if let Some(marker) = episode
        .iter()
        .find(|entry| entry.event == BLOCKED_TASK_TRIAGED_EVENT)
    {
        return TriageCandidacy::Triaged { at: marker.at };
    }
    // A human who set the block made the status change themselves.
    if !is_automation_actor(&history[start].by) {
        return TriageCandidacy::HumanIntervened {
            by: history[start].by.clone(),
        };
    }
    match later_touch(blocked_at, task.updated_at, history, comments, artifacts) {
        Some(LaterTouch::Human { by }) => return TriageCandidacy::HumanIntervened { by },
        Some(LaterTouch::Unexplained { changed_at }) => {
            return TriageCandidacy::UnexplainedChange { changed_at };
        }
        None => {}
    }
    // An in-pipeline escalation blocks the task with its own status entry,
    // which may be stamped after the decision it carries; that entry counts.
    let decision = FinalRecoveryRecord::newest_since(history, comments, blocked_at);
    let basis = match &decision {
        Some(record) if record.outcome == "escalated" => TriageBasis::Escalated,
        Some(record) => {
            return TriageCandidacy::NotEscalated {
                outcome: record.outcome.clone(),
            };
        }
        None if episode.iter().any(is_escalation) => TriageBasis::Escalated,
        None if now - blocked_at > Duration::hours(MAX_EPISODE_AGE_HOURS) => TriageBasis::Undecided,
        None => return TriageCandidacy::AwaitingBackstop,
    };
    let block = current_block(history);
    let run_id = block
        .and_then(blocking_run_id)
        .map(str::to_string)
        .or_else(|| task.job_run_id.clone());
    // The decision comment is written before the status entry that blocks
    // the task, so it is matched by the blocking run, not by time alone.
    let run_field = run_id.as_ref().map(|run_id| format!("run_id={run_id}"));
    let escalation = comments
        .iter()
        .rev()
        .filter(|comment| comment.by == SYSTEM_ACTOR_LABEL)
        .find(|comment| {
            comment.message.lines().next().is_some_and(|header| {
                let mut fields = header.split_whitespace();
                fields.next() == Some("final_recovery")
                    && header
                        .split_whitespace()
                        .any(|field| field == "outcome=escalated")
                    && (comment.at >= blocked_at
                        || run_field
                            .as_deref()
                            .is_some_and(|run| fields.any(|field| field == run)))
            })
        })
        .map(|comment| comment.message.clone());
    TriageCandidacy::Qualifies(TriageEpisode {
        task_id: task.id.clone(),
        blocked_at,
        basis,
        run_id,
        block_note: block
            .and_then(|entry| entry.note.clone())
            .unwrap_or_default(),
        escalation,
    })
}

/// One blocked task as the triage reads it.
#[derive(Debug, Clone)]
pub struct BlockedTriageView {
    /// The blocked task.
    pub task: Task,
    /// Its current episode's candidacy.
    pub candidacy: TriageCandidacy,
}

impl OrbitRuntime {
    /// `task`'s current block episode candidacy at `now`, from its current
    /// history, comments and artifacts: [`triage_candidacy`].
    pub fn block_triage_candidacy_of(
        &self,
        task: &Task,
        now: DateTime<Utc>,
    ) -> Result<TriageCandidacy, OrbitError> {
        let history = self.get_task_history(&task.id)?;
        let comments = self.get_task_comments(&task.id)?;
        let artifacts = self.get_task_artifact_manifest(&task.id)?;
        Ok(triage_candidacy(task, &history, &comments, &artifacts, now))
    }

    /// Every blocked task with its current episode's candidacy, oldest block
    /// first.
    pub fn blocked_triage_view(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<BlockedTriageView>, OrbitError> {
        let mut views = Vec::new();
        for task in
            self.list_tasks_filtered(Some(TaskStatus::Blocked), None, None, None, None, None)?
        {
            let candidacy = self.block_triage_candidacy_of(&task, now)?;
            views.push(BlockedTriageView { task, candidacy });
        }
        views.sort_by_key(|view| match &view.candidacy {
            TriageCandidacy::Qualifies(episode) => (0, episode.blocked_at),
            _ => (1, view.task.updated_at),
        });
        Ok(views)
    }
}

/// The task field a retired triage proposal's option rewrote, as its comment
/// spelled it. Answering a `block_triage` request raised before options
/// carried their own payload still reads it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriageTarget {
    /// The whole acceptance-criteria list, one criterion per line.
    AcceptanceCriteria,
    /// The whole description.
    Description,
}

impl TriageTarget {
    /// The spelling a proposal carried and its comment recorded.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AcceptanceCriteria => "acceptance_criteria",
            Self::Description => "description",
        }
    }

    /// The target `value` spells, if any.
    pub fn parse(value: &str) -> Option<Self> {
        [Self::AcceptanceCriteria, Self::Description]
            .into_iter()
            .find(|target| target.as_str() == value)
    }
}
