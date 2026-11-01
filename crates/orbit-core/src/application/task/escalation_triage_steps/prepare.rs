//! Select the held pilot assessments an escalation triage run reads, draw each
//! one's crew (from the pilot-warning purpose pool, else the hard tier) and
//! partition them by crew.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{Task, TaskComplexity, TaskStatus};
use orbit_types::workflow::automation::members::StateTriggerKind;
use orbit_types::workflow::{ActivityCrewPoolMember, CrewPurpose};
use serde_json::{Value, json};

use super::{HeldPilotWarnings, PilotWarningCandidacy};
use crate::OrbitRuntime;
use crate::application::contracts::{ESCALATION_TRIAGE_JOB, PURPOSE_SOURCE_PREFIX};
use crate::application::task::batch_prepare::{
    BatchSpec, Qualification, held_by_run_detail, prepare_batch,
};
use crate::application::task::pilot::input::requested_workspace_root;
use crate::application::task::pilot::requested_base_branch;
use crate::application::task::pilot::source::{SourceSnapshot, resolve_source_snapshot};
use crate::application::task::{TaskAutomationPort, TaskDistributedPort, TaskJobPort};
use crate::application::task::{TaskListFilter, crew_named_by};

/// Refuting a pilot finding takes the strongest reading of the source, so a
/// triager with no purpose pool to draw from comes from this tier.
const TRIAGE_TIER: TaskComplexity = TaskComplexity::Hard;

/// What one escalation triage run reads: the job's `subject` input. Only a
/// `proposed` task's held pilot assessment (`pilot_warnings_held`) remains;
/// blocked tasks are decided by final recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Subject {
    PilotWarning,
}

impl Subject {
    fn parse(action: &str, input: &Value) -> Result<Self, DispatchError> {
        match input.get("subject") {
            None | Some(Value::Null) => Ok(Self::PilotWarning),
            Some(Value::String(subject)) => match subject.as_str() {
                "" | "pilot_warning" => Ok(Self::PilotWarning),
                "block" => Err(DispatchError::action_failed(
                    action,
                    "the `block` subject is retired: final recovery decides blocked tasks and \
                     opens the decision request an operator answers. Disable or delete the \
                     `blocked-task-triage` routine; only `pilot_warning` remains",
                )),
                other => Err(DispatchError::action_failed(
                    action,
                    format!("subject `{other}` is not pilot_warning"),
                )),
            },
            Some(_) => Err(DispatchError::action_failed(
                action,
                "subject must be the string pilot_warning",
            )),
        }
    }

    fn as_str(self) -> &'static str {
        "pilot_warning"
    }

    /// The trigger kind whose members are this subject.
    fn trigger_kind(self) -> StateTriggerKind {
        StateTriggerKind::PilotWarningsHeld
    }

    /// The purpose whose `workflow.purpose_crews` pool this subject's
    /// triagers are drawn from while it is set.
    fn purpose(self) -> Option<CrewPurpose> {
        Some(CrewPurpose::PilotWarningTriage)
    }

    /// The status a candidate of this subject holds.
    fn status(self) -> TaskStatus {
        TaskStatus::Proposed
    }
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
    let claim = runtime.state_claim(input).map_err(failed)?;
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
    // Triagers cite the source to dispute a finding, so a Git run pins one
    // revision as finding verification does. A non-Git workspace pins none,
    // as a task pilot does, and its triagers report no observed revision.
    let source = resolve_source_snapshot(runtime, action, input, &workspace_root)?;
    prepare_batch(
        runtime,
        action,
        input,
        run_id,
        BatchSpec {
            claim,
            source_revision: source.as_ref().map(|source| source.source_revision.clone()),
            source: source
                .as_ref()
                .map(SourceSnapshot::to_json)
                .unwrap_or_else(|| {
                    json!({
                        "base_branch": requested_base_branch(runtime, input),
                        "source_ref": Value::Null,
                        "source_revision": Value::Null,
                        "fast_forwarded": false,
                    })
                }),
            workspace_path: workspace_root,
            automatic_filter: TaskListFilter {
                statuses: Some(vec![subject.status()]),
                ..Default::default()
            },
            // A task in the subject's status that holds nothing is not a
            // candidate at all.
            silent_reasons: &["not_held"],
            before_workspace: vec![("subject", json!(subject.as_str()))],
            before_task_count: Vec::new(),
        },
        |runtime| runtime.escalation_triage_holders(),
        |_| true,
        |task, held, _explicit| {
            if held.contains_key(&task.id) {
                return Ok(Qualification::Rejected("already_triaging"));
            }
            match qualify(runtime, subject, task)? {
                Ok((row, author)) => Ok(Qualification::Selected((row, author))),
                Err(reason) => Ok(Qualification::Rejected(reason)),
            }
        },
        |task, (mut row, author): (Value, Option<String>)| {
            let triager = triager(runtime, subject, task, author.as_deref())?;
            let crew = triager.crew.clone();
            row["crew"] = json!(crew);
            row["tier"] = json!(triager.tier);
            row["crew_source"] = json!(triager.source);
            row["eligible_pool"] = json!(triager.eligible_pool);
            Ok((row, crew))
        },
        skipped_detail,
        |tasks, task_ids| {
            let draws = tasks
                .iter()
                .filter(|task| task_ids.iter().any(|id| task["task_id"] == *id))
                .map(|task| {
                    json!({
                        "task_id": task["task_id"],
                        "source": task["crew_source"],
                        "eligible_pool": task["eligible_pool"],
                    })
                })
                .collect::<Vec<_>>();
            Some(("draws", json!(draws)))
        },
    )
}

/// `task`'s prepared row and the crew to pass over when it qualifies for
/// `subject`, else the stable reason it does not.
fn qualify(
    runtime: &OrbitRuntime,
    subject: Subject,
    task: &Task,
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
        .stored_run_state(run_id)?
        .as_ref()
        .and_then(|state| runtime.pilot_assessment_crew(state)))
}

/// One triager draw: the crew, where it was drawn from and the weights the
/// draw ran on.
struct TriagerDraw {
    crew: String,
    /// The complexity tier drawn from; `None` for a purpose-pool draw.
    tier: Option<TaskComplexity>,
    /// `purpose:<purpose>`, `pool:<tier>`, or `default`.
    source: String,
    eligible_pool: Vec<ActivityCrewPoolMember>,
}

/// `task`'s triager: a draw from the subject's `workflow.purpose_crews` pool,
/// when it has one that is set and holds a member that can inspect source
/// natively, else the hard tier's inspection-capable draw, else the
/// inspection-capable default crew, exactly as a finding verifier is drawn.
/// Either draw passes over the assessment's author while the pool offers
/// another. With none eligible, preparation fails with
/// `inspection_crew_unavailable` before any triager launches.
fn triager(
    runtime: &OrbitRuntime,
    subject: Subject,
    task: &Task,
    author: Option<&str>,
) -> Result<TriagerDraw, OrbitError> {
    let avoid = author.into_iter().collect::<Vec<_>>();
    if let Some(purpose) = subject.purpose()
        && let Some((candidates, _)) = runtime.purpose_pool_candidates(purpose, true)?
    {
        let preferred = candidates
            .iter()
            .filter(|candidate| {
                !avoid
                    .iter()
                    .any(|label| crew_named_by(&candidate.crew, label))
            })
            .cloned()
            .collect::<Vec<_>>();
        let candidates = if preferred.is_empty() {
            candidates
        } else {
            preferred
        };
        let source = format!("{PURPOSE_SOURCE_PREFIX}{purpose}");
        let mut ticket = runtime
            .seeded_crew_draw_tickets(format!("{ESCALATION_TRIAGE_JOB}:{}:{purpose}", task.id));
        let draw = runtime.crew_pool_draw(candidates, &source, &mut *ticket)?;
        return Ok(TriagerDraw {
            crew: draw.crew.name,
            tier: None,
            source,
            eligible_pool: draw.eligible_pool,
        });
    }
    if let Some(assignment) = runtime.draw_finding_verifier(task, TRIAGE_TIER, &avoid)? {
        return Ok(TriagerDraw {
            source: format!("pool:{}", assignment.tier),
            crew: assignment.crew,
            tier: Some(assignment.tier),
            eligible_pool: assignment.eligible_pool,
        });
    }
    let crew = runtime.inspection_default_crew(TRIAGE_TIER)?;
    Ok(TriagerDraw {
        crew: crew.name,
        tier: Some(TRIAGE_TIER),
        source: "default".to_string(),
        eligible_pool: Vec::new(),
    })
}

fn skipped_detail(reason: &str, holders: Option<&BTreeSet<String>>) -> String {
    match reason {
        "already_triaging" => held_by_run_detail("triaged", holders),
        "not_held" => {
            "the task is not proposed, or its latest task-pilot assessment holds nothing on \
             warnings"
                .into()
        }
        "operator_decided" => {
            "an operator recorded a task-pilot admission decision on the latest assessment".into()
        }
        "triaged" => "the latest assessment was already triaged".into(),
        _ => reason.into(),
    }
}
