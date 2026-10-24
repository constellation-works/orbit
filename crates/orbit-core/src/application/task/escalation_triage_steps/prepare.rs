//! Select the escalated holds an escalation triage run reads (held pilot
//! assessments, or block episodes awaiting triage), draw each one's
//! hard-tier crew and partition them by crew.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{Task, TaskComplexity, TaskStatus};
use orbit_types::workflow::automation::members::StateTriggerKind;
use serde_json::{Value, json};

use super::{HeldPilotWarnings, PilotWarningCandidacy};
use crate::OrbitRuntime;
use crate::application::task::TriageCandidacy;
use crate::application::task::pilot::input::{
    bounded_usize, requested_workspace_root, string_array,
};
use crate::application::task::pilot::source::resolve_source_snapshot;
use crate::application::task::{TaskListFilter, VerifierAssignment};

const DEFAULT_MAX_PARTITION_SIZE: usize = 5;
const HARD_MAX_PARTITION_SIZE: usize = 5;
const DEFAULT_MAX_TASKS: usize = 50;
const HARD_MAX_TASKS: usize = 500;

/// Refuting a pilot finding takes the strongest reading of the source, so
/// every triager is drawn from this tier.
const TRIAGE_TIER: TaskComplexity = TaskComplexity::Hard;

/// What one escalation triage run reads: the job's `subject` input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Subject {
    /// A `proposed` task's held pilot assessment (`pilot_warnings_held`).
    PilotWarning,
    /// A `blocked` task's block episode awaiting triage (`block_escalated`).
    Block,
}

impl Subject {
    fn parse(action: &str, input: &Value) -> Result<Self, DispatchError> {
        match input.get("subject") {
            None | Some(Value::Null) => Ok(Self::PilotWarning),
            Some(Value::String(subject)) => match subject.as_str() {
                "" | "pilot_warning" => Ok(Self::PilotWarning),
                "block" => Ok(Self::Block),
                other => Err(DispatchError::action_failed(
                    action,
                    format!("subject `{other}` is not one of pilot_warning or block"),
                )),
            },
            Some(_) => Err(DispatchError::action_failed(
                action,
                "subject must be a string: pilot_warning or block",
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::PilotWarning => "pilot_warning",
            Self::Block => "block",
        }
    }

    /// The trigger kind whose members are this subject.
    fn trigger_kind(self) -> StateTriggerKind {
        match self {
            Self::PilotWarning => StateTriggerKind::PilotWarningsHeld,
            Self::Block => StateTriggerKind::BlockEscalated,
        }
    }

    /// The status a candidate of this subject holds.
    fn status(self) -> TaskStatus {
        match self {
            Self::PilotWarning => TaskStatus::Proposed,
            Self::Block => TaskStatus::Blocked,
        }
    }
}

/// One selected task's prepared row and the crew that wrote what holds it,
/// when one is known.
struct Selected {
    task: Task,
    row: Value,
    author: Option<String>,
}

pub(crate) fn prepare(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    run_id: Option<&str>,
) -> Result<Value, DispatchError> {
    let failed = |error: OrbitError| DispatchError::action_failed(action, error.to_string());
    let subject = Subject::parse(action, input)?;
    let workspace_root = requested_workspace_root(runtime, action, input)?;
    let claim = crate::application::automation::members::claim(runtime, input).map_err(failed)?;
    if let Some(claim) = &claim
        && claim.kind != subject.trigger_kind()
    {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "the state claim's members are not the `{}` subject",
                subject.as_str()
            ),
        ));
    }
    // Triagers cite the source to dispute a finding, so the run pins one
    // revision as finding verification does.
    let source =
        resolve_source_snapshot(runtime, action, input, &workspace_root)?.ok_or_else(|| {
            DispatchError::action_failed(
                action,
                "escalation triage pins a Git revision; the workspace is not a Git work tree",
            )
        })?;
    if let Some(claim) = &claim
        && (source.source_revision != claim.member.source.commit
            || input.get("task_ids") != Some(&json!(claim.task_ids())))
    {
        return Err(DispatchError::action_failed(
            action,
            "state-trigger source or task membership changed",
        ));
    }
    let max_partition_size = bounded_usize(
        action,
        input,
        "max_partition_size",
        DEFAULT_MAX_PARTITION_SIZE,
        HARD_MAX_PARTITION_SIZE,
    )?;
    let max_tasks = bounded_usize(
        action,
        input,
        "max_tasks",
        DEFAULT_MAX_TASKS,
        HARD_MAX_TASKS,
    )?;
    let explicit_task_ids = string_array(input, "task_ids", action)?;
    let explicit = !explicit_task_ids.is_empty();
    if explicit_task_ids.len() > max_tasks {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "explicit selection names {} tasks, exceeding max_tasks {max_tasks}",
                explicit_task_ids.len()
            ),
        ));
    }

    // A retried prepare is not held by its own earlier checkpoint.
    let mut held = crate::application::automation::preparation::active_escalation_triages(runtime)
        .map_err(failed)?;
    if let Some(run_id) = run_id {
        held.retain(|_, holders| {
            holders.remove(run_id);
            !holders.is_empty()
        });
    }

    let candidates = if explicit {
        let mut seen = BTreeSet::new();
        explicit_task_ids
            .iter()
            .map(|task_id| {
                if !seen.insert(task_id.as_str()) {
                    return Err(DispatchError::action_failed(
                        action,
                        format!("explicit task_ids contains duplicate {task_id}"),
                    ));
                }
                runtime.get_task(task_id).map_err(|error| {
                    if matches!(error, OrbitError::NotFound { .. }) {
                        DispatchError::action_failed(
                            action,
                            format!("task {task_id} does not exist in the selected workspace"),
                        )
                    } else {
                        failed(error)
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let mut envelopes = runtime
            .task_candidates(
                &TaskListFilter {
                    statuses: Some(vec![subject.status()]),
                    ..Default::default()
                },
                usize::MAX,
            )
            .map_err(failed)?
            .items;
        // Oldest holds first, so a capped pass makes progress through them.
        envelopes.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.id.cmp(&right.id))
        });
        envelopes
            .iter()
            .map(|envelope| runtime.get_task(&envelope.id))
            .collect::<Result<Vec<_>, _>>()
            .map_err(failed)?
    };

    // An explicitly named task that no longer qualifies settles superseded
    // with its reason, so a claimed member is released rather than failed.
    let now = Utc::now();
    let mut superseded = Vec::new();
    let mut excluded = BTreeMap::<&'static str, usize>::new();
    let mut selected = Vec::new();
    for task in candidates {
        let reason = if held.contains_key(&task.id) {
            Err("already_triaging")
        } else {
            qualify(runtime, subject, &task, now).map_err(failed)?
        };
        match reason {
            Ok(_) if selected.len() >= max_tasks => {
                *excluded.entry("deferred_by_max_tasks").or_default() += 1;
            }
            Ok((row, author)) => selected.push(Selected { task, row, author }),
            Err(reason) if explicit => superseded.push(json!({
                "task_id": task.id, "outcome": "superseded", "reason": reason,
                "status": task.status,
                "detail": skipped_detail(reason, held.get(&task.id)),
            })),
            // A task in the subject's status that holds nothing is not a
            // candidate at all.
            Err("not_held" | "not_blocked") => {}
            Err(reason) => *excluded.entry(reason).or_default() += 1,
        }
    }

    // Every task's triager is drawn before any is dispatched, so its crew is
    // part of the prepared checkpoint the run records.
    let mut tasks = Vec::with_capacity(selected.len());
    let mut by_crew = BTreeMap::<String, Vec<String>>::new();
    for Selected {
        task,
        mut row,
        author,
    } in selected
    {
        let triager = triager(runtime, &task, author.as_deref()).map_err(failed)?;
        by_crew
            .entry(triager.crew.clone())
            .or_default()
            .push(task.id.clone());
        row["crew"] = json!(triager.crew);
        row["tier"] = json!(triager.tier);
        tasks.push(row);
    }
    let partitions = by_crew
        .iter()
        .flat_map(|(crew, ids)| {
            ids.chunks(max_partition_size)
                .map(move |chunk| (crew.clone(), chunk.to_vec()))
        })
        .enumerate()
        .map(|(partition_index, (crew, task_ids))| {
            json!({"partition_index": partition_index, "task_ids": task_ids, "crew": crew})
        })
        .collect::<Vec<_>>();
    let task_ids = tasks
        .iter()
        .map(|task| task["task_id"].clone())
        .collect::<Vec<_>>();

    Ok(json!({
        "state_automation": claim,
        "mode": if explicit { "explicit" } else { "automatic" },
        "subject": subject.as_str(),
        "workspace_path": workspace_root,
        "owner_machine": runtime.automation_execution_location(),
        "source": source.to_json(),
        "task_count": task_ids.len(),
        "task_ids": task_ids,
        "tasks": tasks,
        "partition_size": max_partition_size,
        "partition_count": partitions.len(),
        "partitions": partitions,
        "excluded_by_reason": excluded,
        "superseded_during_preparation": superseded,
    }))
}

/// `task`'s prepared row and the crew to pass over when it qualifies for
/// `subject` at `now`, else the stable reason it does not.
fn qualify(
    runtime: &OrbitRuntime,
    subject: Subject,
    task: &Task,
    now: chrono::DateTime<Utc>,
) -> Result<Result<(Value, Option<String>), &'static str>, OrbitError> {
    Ok(match subject {
        Subject::PilotWarning => match runtime.pilot_warning_candidacy_of(task)? {
            PilotWarningCandidacy::Qualifies(warnings) => {
                let author = assessment_author(runtime, &warnings)?;
                Ok((
                    json!({
                        "task_id": task.id,
                        "title": task.title,
                        "status": task.status,
                        "operation_id": warnings.operation_id,
                        "pilot_run_id": warnings.pilot_run_id,
                        "findings": warnings.findings,
                        "assessment_crew": author,
                    }),
                    author,
                ))
            }
            other => Err(other.as_str()),
        },
        // The shared triage rule, exactly as the tool and the trigger apply
        // it. No crew wrote a block, so the draw passes over none.
        Subject::Block => match runtime.block_triage_candidacy_of(task, now)? {
            TriageCandidacy::Qualifies(episode) => Ok((
                json!({
                    "task_id": task.id,
                    "title": task.title,
                    "status": task.status,
                    "episode": episode.key(),
                    "blocked_at": episode.blocked_at.to_rfc3339_opts(SecondsFormat::Secs, true),
                    "basis": episode.basis,
                    "run_id": episode.run_id,
                }),
                None,
            )),
            other => Err(other.as_str()),
        },
    })
}

/// The crew that wrote the held assessment: the pilot run's frozen draw.
/// `None` when the audit names no run, or the run's state is gone.
fn assessment_author(
    runtime: &OrbitRuntime,
    warnings: &HeldPilotWarnings,
) -> Result<Option<String>, OrbitError> {
    let Some(run_id) = &warnings.pilot_run_id else {
        return Ok(None);
    };
    Ok(runtime
        .read_run_state(run_id)?
        .as_ref()
        .and_then(crate::application::distributed::pilot_run_crew))
}

/// `task`'s triager: a hard-tier draw passing over the assessment's author
/// while the pool offers another, else the default crew, exactly as a
/// finding verifier is drawn. Any configured crew may triage; the
/// observed-revision proof on its `orbit.task.block_triage` decisions, not
/// the draw, establishes that it read the pinned source.
fn triager(
    runtime: &OrbitRuntime,
    task: &Task,
    author: Option<&str>,
) -> Result<VerifierAssignment, OrbitError> {
    let avoid = author.into_iter().collect::<Vec<_>>();
    if let Some(assignment) = runtime.draw_finding_verifier(task, TRIAGE_TIER, &avoid)? {
        return Ok(assignment);
    }
    let crew = runtime.resolve_crew_for_task(None, None)?;
    Ok(VerifierAssignment {
        crew: crew.name,
        tier: TRIAGE_TIER,
        eligible_pool: Vec::new(),
    })
}

fn skipped_detail(reason: &str, holders: Option<&BTreeSet<String>>) -> String {
    match reason {
        "already_triaging" => format!(
            "triaged by run {}",
            holders
                .into_iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "not_held" => {
            "the task is not proposed, or its latest task-pilot assessment holds nothing on \
             warnings"
                .into()
        }
        "operator_decided" => {
            "an operator recorded a task-pilot admission decision on the latest assessment".into()
        }
        "triaged" => "the latest assessment or block episode was already triaged".into(),
        "human_intervened" => "someone outside Orbit's automation acted after the block".into(),
        "unexplained_change" => "the task changed after the block with no recorded actor".into(),
        "not_escalated" => "the block's final-recovery decision is not an escalation".into(),
        "awaiting_backstop" => {
            "no decision yet; the blocked-task backstop may still recover it".into()
        }
        "no_episode" => "no history entry records the block".into(),
        "not_blocked" => "the task is not blocked".into(),
        _ => reason.into(),
    }
}
