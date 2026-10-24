//! The blocked-task triage: which block episodes it reads, and the one
//! decision it records on each.
//!
//! Every block already gets one recovery decision — final recovery in the
//! pipeline, or the backstop outside it. What stays blocked is what that
//! decision escalated, plus blocks the backstop never decides (older than
//! [`MAX_EPISODE_AGE_HOURS`]). The `blocked-task-triage` auto-task reads each
//! such episode once and records one of three dispositions through
//! [`OrbitRuntime::record_block_triage`]:
//!
//! - **requeue** (mechanical): back to `backlog` as a final-recovery requeue,
//!   counted against the same [`FinalRecoveryRequeueBound`] and recorded with
//!   the same event, so final recovery and triage share one count. A requeue
//!   past the bound is refused and recorded; an unreadable count refuses
//!   without writing.
//! - **propose** (decision): one comment with the quoted blocker, the
//!   options, a recommendation and ready-to-apply replacement text. Nothing
//!   else on the task changes.
//! - **report** (human-only): one comment naming the human action.
//!
//! Each records a [`BLOCKED_TASK_TRIAGED_EVENT`] in the task history, which
//! is what marks the episode triaged. A requeue never applies to a task
//! tagged `security` or `security-review`, or whose run executed on another
//! machine: those are recorded as refused, with the human action, because
//! they are the operator's call.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    ArtifactManifestFileV2, FINAL_RECOVERY_DECIDED_EVENT, RecordedFinalRecovery, Task, TaskComment,
    TaskDecision, TaskHistoryEntry, TaskStatus,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::super::TaskRecordUpdateParams as StoreTaskUpdateParams;
use super::super::final_recovery::{FINAL_RECOVERY_REQUEUED_EVENT, FinalRecoveryRequeueBound};
use super::super::helpers::{SYSTEM_ACTOR_LABEL, is_automation_actor};
use super::MAX_EPISODE_AGE_HOURS;
use super::episode::{FinalRecoveryRecord, LaterTouch, later_touch};
use super::readout::{blocking_run_id, current_block, is_escalation};
use crate::OrbitRuntime;

/// History event that marks a block episode triaged.
pub const BLOCKED_TASK_TRIAGED_EVENT: &str = "blocked_task_triaged";

/// Tags whose tasks only an operator requeues.
const OPERATOR_ONLY_TAGS: [&str; 2] = ["security", "security-review"];

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

    fn detail(&self) -> String {
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

/// What the triage asks to record on one episode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockTriageDecision {
    /// Mechanical: the cause was repaired; return the task to `backlog`.
    Requeue {
        /// The repair applied and why the task can run again.
        repair: String,
    },
    /// Decision: the operator chooses between options.
    Propose {
        /// The options, two or three.
        options: Vec<TriageOption>,
        /// 1-based index of the recommended option.
        recommended: usize,
    },
    /// Human-only: name the action only a human can take.
    Report {
        /// The specific human action.
        human_action: String,
    },
}

/// One option of a decision proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriageOption {
    /// What the option decides.
    pub summary: String,
    /// What choosing it leads to.
    pub consequences: String,
    /// The exact replacement text for each changed description section or
    /// criterion, ready to apply.
    pub replacement: String,
}

/// A triage decision for one task's current episode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockTriageRequest {
    /// The blocked task.
    pub task_id: String,
    /// The escalation reason, quoted in the comment.
    pub blocker: String,
    /// The decision.
    pub decision: BlockTriageDecision,
    /// Who asked, named in the comment.
    pub requested_by: String,
}

/// What [`OrbitRuntime::record_block_triage`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TriageWrite {
    disposition: &'static str,
    outcome: &'static str,
    comment: String,
    requeue: Option<(usize, FinalRecoveryRequeueBound)>,
    refusal: Option<String>,
}

impl OrbitRuntime {
    /// Every blocked task with its current episode's triage candidacy,
    /// oldest block first.
    pub fn blocked_triage_view(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<BlockedTriageView>, OrbitError> {
        let mut views = Vec::new();
        for task in
            self.list_tasks_filtered(Some(TaskStatus::Blocked), None, None, None, None, None)?
        {
            let history = self.get_task_history(&task.id)?;
            let comments = self.get_task_comments(&task.id)?;
            let artifacts = self.get_task_artifact_manifest(&task.id)?;
            let candidacy = triage_candidacy(&task, &history, &comments, &artifacts, now);
            views.push(BlockedTriageView { task, candidacy });
        }
        views.sort_by_key(|view| match &view.candidacy {
            TriageCandidacy::Qualifies(episode) => (0, episode.blocked_at),
            _ => (1, view.task.updated_at),
        });
        Ok(views)
    }

    /// The triage's candidate list as JSON: every blocked task it read, with
    /// what makes each qualifying episode actionable or human-only.
    pub fn blocked_triage_candidates_json(&self, now: DateTime<Utc>) -> Result<Value, OrbitError> {
        let bound = FinalRecoveryRequeueBound::default();
        let views = self.blocked_triage_view(now)?;
        let mut qualifying = Vec::new();
        let mut excluded = Vec::new();
        for view in &views {
            let task = &view.task;
            match &view.candidacy {
                TriageCandidacy::Qualifies(episode) => {
                    let requeues = self.recent_requeues(&task.id, bound.window);
                    qualifying.push(json!({
                        "task_id": task.id,
                        "title": task.title,
                        "blocked_at": episode.blocked_at.to_rfc3339_opts(SecondsFormat::Secs, true),
                        "block_age_hours": (now - episode.blocked_at).num_hours(),
                        "basis": episode.basis,
                        "run_id": episode.run_id,
                        "block_note": episode.block_note,
                        "escalation": episode.escalation,
                        "tags": task.tags,
                        "job_run_machine": task.job_run_machine.as_ref().map(|location| &location.machine_id),
                        "run_is_local": self.task_run_is_local(task),
                        "requeue_refusal": operator_only_requeue(self, task),
                        "requeues_in_window": requeues.as_ref().ok(),
                        "requeue_count_error": requeues.as_ref().err().map(ToString::to_string),
                        "requeue_bound": bound_json(bound),
                    }));
                }
                other => excluded.push(json!({
                    "task_id": task.id,
                    "why": other.as_str(),
                    "detail": other.detail(),
                })),
            }
        }
        Ok(json!({
            "blocked": views.len(),
            "qualifying": qualifying,
            "excluded": excluded,
        }))
    }

    /// Record the triage's decision on `request.task_id`'s current block
    /// episode, under the task write lock. Refused unless that episode
    /// [qualifies](triage_candidacy). See the module docs for what each
    /// decision writes.
    pub fn record_block_triage(&self, request: &BlockTriageRequest) -> Result<Value, OrbitError> {
        let blocker = redact_all(request.blocker.trim());
        if blocker.is_empty() {
            return Err(OrbitError::InvalidInput(
                "block triage needs `blocker`: quote the escalation reason".into(),
            ));
        }
        let decision = validated(&request.decision)?;
        self.ensure_coordination_task_write_permitted()?;
        let id = request.task_id.as_str();
        let mut answer = None;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let task = self.get_task(id)?;
            let history = self.get_task_history(id)?;
            let comments = self.get_task_comments(id)?;
            let artifacts = self.get_task_artifact_manifest(id)?;
            let episode =
                match triage_candidacy(&task, &history, &comments, &artifacts, Utc::now()) {
                    TriageCandidacy::Qualifies(episode) => episode,
                    other => {
                        return Err(OrbitError::InvalidInput(format!(
                            "{id} has no block episode awaiting triage: {}; nothing was recorded",
                            other.detail()
                        )));
                    }
                };
            let write = self.plan_triage(&task, &episode, &blocker, &decision, request)?;
            self.write_triage(&task, &episode, &write)?;
            answer = Some(json!({
                "task_id": id,
                "episode": episode.blocked_at.to_rfc3339_opts(SecondsFormat::Nanos, true),
                "disposition": write.disposition,
                "outcome": write.outcome,
                "status": if write.outcome == "requeued" { TaskStatus::Backlog } else { TaskStatus::Blocked },
                "requeue": write.requeue.map(|(count, bound)| json!({
                    "count": count,
                    "max_requeues": bound.max_requeues,
                    "window_hours": bound.window.num_hours(),
                    "summary": requeue_summary(count, bound),
                })),
                "refusal": write.refusal,
                "comment": write.comment,
            }));
            Ok(())
        })?;
        answer.ok_or_else(|| OrbitError::Execution("block triage recorded nothing".into()))
    }

    fn plan_triage(
        &self,
        task: &Task,
        episode: &TriageEpisode,
        blocker: &str,
        decision: &BlockTriageDecision,
        request: &BlockTriageRequest,
    ) -> Result<TriageWrite, OrbitError> {
        let header = |disposition: &str, outcome: &str| {
            format!(
                "blocked_task_triage disposition={disposition} outcome={outcome} episode={} by={}",
                episode
                    .blocked_at
                    .to_rfc3339_opts(SecondsFormat::Secs, true),
                request.requested_by
            )
        };
        let quoted = quote(blocker);
        Ok(match decision {
            BlockTriageDecision::Requeue { repair } => {
                let refused = |reason: String, human_action: &str| TriageWrite {
                    disposition: "mechanical",
                    outcome: "refused",
                    comment: format!(
                        "{}\nblocker:\n{quoted}\nrepair: {repair}\nreason: {reason}\nhuman_action: {human_action}",
                        header("mechanical", "refused")
                    ),
                    requeue: None,
                    refusal: Some(reason),
                };
                if let Some(reason) = operator_only_requeue(self, task) {
                    return Ok(refused(
                        reason,
                        "an operator decides whether this task runs again",
                    ));
                }
                if let Some(reason) = self.unsettled_run(task)? {
                    return Err(OrbitError::InvalidInput(format!(
                        "{} cannot be requeued yet: {reason}; nothing was recorded",
                        task.id
                    )));
                }
                let bound = FinalRecoveryRequeueBound::default();
                // Fail closed: without the shared count no requeue happens.
                let recent = self
                    .recent_requeues(&task.id, bound.window)
                    .map_err(|error| {
                        OrbitError::Execution(format!(
                            "requeue refused: the final-recovery requeue count for {} cannot be \
                         read ({error}); nothing was requeued or recorded, so report the block",
                            task.id
                        ))
                    })?;
                if recent >= bound.max_requeues {
                    return Ok(refused(
                        format!(
                            "requeue refused: this task was already requeued {recent} time(s) in \
                             the last {} hour(s) (limit {} per {} hour(s), shared with final \
                             recovery)",
                            bound.window.num_hours(),
                            bound.max_requeues,
                            bound.window.num_hours()
                        ),
                        "the failure keeps recurring; fix its cause before moving the task back \
                         to backlog",
                    ));
                }
                TriageWrite {
                    disposition: "mechanical",
                    outcome: "requeued",
                    comment: format!(
                        "{} {}\nblocker:\n{quoted}\nrepair: {repair}",
                        header("mechanical", "requeued"),
                        requeue_summary(recent + 1, bound)
                    ),
                    requeue: Some((recent + 1, bound)),
                    refusal: None,
                }
            }
            BlockTriageDecision::Propose {
                options,
                recommended,
            } => TriageWrite {
                disposition: "decision",
                outcome: "proposed",
                comment: format!(
                    "{}\nblocker:\n{quoted}{}\nNothing else on this task was changed: apply the \
                     chosen replacement text with `orbit task update`, then move the task back to \
                     backlog.",
                    header("decision", "proposed"),
                    proposal_text(options, *recommended)
                ),
                requeue: None,
                refusal: None,
            },
            BlockTriageDecision::Report { human_action } => TriageWrite {
                disposition: "human_only",
                outcome: "reported",
                comment: format!(
                    "{}\nblocker:\n{quoted}\nhuman_action: {human_action}",
                    header("human_only", "reported")
                ),
                requeue: None,
                refusal: None,
            },
        })
    }

    fn write_triage(
        &self,
        task: &Task,
        episode: &TriageEpisode,
        write: &TriageWrite,
    ) -> Result<(), OrbitError> {
        let now = Utc::now();
        let comment = TaskComment {
            at: now,
            by: SYSTEM_ACTOR_LABEL.to_string(),
            message: write.comment.clone(),
            operation_id: None,
        };
        let marker = TaskHistoryEntry {
            at: now,
            by: SYSTEM_ACTOR_LABEL.to_string(),
            event: BLOCKED_TASK_TRIAGED_EVENT.to_string(),
            note: Some(format!(
                "disposition={} outcome={} episode={}",
                write.disposition,
                write.outcome,
                episode
                    .blocked_at
                    .to_rfc3339_opts(SecondsFormat::Nanos, true)
            )),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: None,
        };
        let requeued = write.requeue.is_some();
        let mut history = Vec::new();
        // A requeue is the final-recovery requeue of the blocking run: the
        // same typed decision and publication record the applier writes, so
        // the next run resumes exactly what a final-recovery requeue resumes.
        if requeued && let Some(run_id) = &episode.run_id {
            history.push(TaskHistoryEntry {
                at: now,
                by: SYSTEM_ACTOR_LABEL.to_string(),
                event: FINAL_RECOVERY_DECIDED_EVENT.to_string(),
                note: write.comment.lines().next().map(str::to_string),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: Some(TaskDecision::FinalRecovery {
                    run_id: run_id.clone(),
                    decision: "blocked_task_triage_requeue".to_string(),
                    outcome: RecordedFinalRecovery::Requeued,
                }),
            });
            history.extend(self.published_owner_requeue_record(task, run_id)?);
        }
        history.push(marker);
        let status_note = requeued.then(|| match &episode.run_id {
            Some(run_id) => format!(
                "final recovery (run_id={run_id}): blocked-task triage requeue: {}",
                requeue_note(&write.comment)
            ),
            None => format!(
                "blocked-task triage requeue: {}",
                requeue_note(&write.comment)
            ),
        });
        self.with_mutation(|| {
            let updated = self.stores().task_records().update(
                &task.id,
                StoreTaskUpdateParams {
                    actor: SYSTEM_ACTOR_LABEL.to_string(),
                    status: requeued.then_some(TaskStatus::Backlog),
                    status_event: requeued.then(|| FINAL_RECOVERY_REQUEUED_EVENT.to_string()),
                    status_note: status_note.clone(),
                    append_comments: vec![comment.clone()],
                    append_history: history.clone(),
                    expected_status: Some(vec![TaskStatus::Blocked]),
                    ..Default::default()
                },
            )?;
            Ok((
                updated,
                OrbitEvent::TaskUpdated {
                    id: task.id.clone(),
                },
            ))
        })?;
        Ok(())
    }
}

/// Why only an operator may requeue `task`, when only one may: a security
/// tag, or a run another machine executed whose evidence this owner cannot
/// read.
fn operator_only_requeue(runtime: &OrbitRuntime, task: &Task) -> Option<String> {
    if let Some(tag) = task
        .tags
        .iter()
        .find(|tag| OPERATOR_ONLY_TAGS.contains(&tag.as_str()))
    {
        return Some(format!(
            "requeue refused: the task is tagged `{tag}`, which only an operator requeues"
        ));
    }
    if !runtime.task_run_is_local(task) {
        let machine = task
            .job_run_machine
            .as_ref()
            .map_or("another machine", |location| location.machine_id.as_str());
        return Some(format!(
            "requeue refused: the blocking run executed on `{machine}`, not this host, so its \
             evidence cannot be read here"
        ));
    }
    None
}

fn validated(decision: &BlockTriageDecision) -> Result<BlockTriageDecision, OrbitError> {
    let text = triage_text;
    Ok(match decision {
        BlockTriageDecision::Requeue { repair } => BlockTriageDecision::Requeue {
            repair: text(repair, "reason")?,
        },
        BlockTriageDecision::Report { human_action } => BlockTriageDecision::Report {
            human_action: text(human_action, "reason")?,
        },
        BlockTriageDecision::Propose {
            options,
            recommended,
        } => BlockTriageDecision::Propose {
            options: validated_proposal(options, *recommended)?,
            recommended: *recommended,
        },
    })
}

/// A decision proposal's options, trimmed and redacted: two or three, each
/// with every text non-empty, and `recommended` naming one of them (1-based).
/// Shared by every triage subject that proposes.
pub(crate) fn validated_proposal(
    options: &[TriageOption],
    recommended: usize,
) -> Result<Vec<TriageOption>, OrbitError> {
    if !(2..=3).contains(&options.len()) {
        return Err(OrbitError::InvalidInput(format!(
            "a decision proposal needs two or three `options`, not {}",
            options.len()
        )));
    }
    if !(1..=options.len()).contains(&recommended) {
        return Err(OrbitError::InvalidInput(format!(
            "`recommended` must name one of the {} options (1-based)",
            options.len()
        )));
    }
    options
        .iter()
        .map(|option| {
            Ok(TriageOption {
                summary: triage_text(&option.summary, "options[].summary")?,
                consequences: triage_text(&option.consequences, "options[].consequences")?,
                replacement: triage_text(&option.replacement, "options[].replacement")?,
            })
        })
        .collect()
}

/// `value` trimmed and redacted, refused when that leaves nothing.
pub(crate) fn triage_text(value: &str, field: &str) -> Result<String, OrbitError> {
    let value = redact_all(value.trim());
    if value.is_empty() {
        Err(OrbitError::InvalidInput(format!(
            "block triage needs a non-empty `{field}`"
        )))
    } else {
        Ok(value)
    }
}

/// `text` as a Markdown quote, line by line.
pub(crate) fn quote(text: &str) -> String {
    text.lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The options and recommendation of a validated proposal, as a proposal
/// comment lists them.
pub(crate) fn proposal_text(options: &[TriageOption], recommended: usize) -> String {
    let mut text = String::new();
    for (index, option) in options.iter().enumerate() {
        text.push_str(&format!(
            "\n\nOption {}: {}\nConsequences: {}\nReplacement text:\n{}",
            index + 1,
            option.summary,
            option.consequences,
            option.replacement
        ));
    }
    text.push_str(&format!(
        "\n\nRecommended: option {recommended} ({})",
        options[recommended - 1].summary
    ));
    text
}

/// `requeue n of 2 per 24h`, as the comment and the run summary state it.
fn requeue_summary(count: usize, bound: FinalRecoveryRequeueBound) -> String {
    format!(
        "requeue {count} of {} per {}h",
        bound.max_requeues,
        bound.window.num_hours()
    )
}

fn bound_json(bound: FinalRecoveryRequeueBound) -> Value {
    json!({
        "max_requeues": bound.max_requeues,
        "window_hours": bound.window.num_hours(),
    })
}

/// The repair line of a requeue comment, for the status note.
fn requeue_note(comment: &str) -> &str {
    comment
        .lines()
        .find_map(|line| line.strip_prefix("repair: "))
        .unwrap_or("requeued")
}
