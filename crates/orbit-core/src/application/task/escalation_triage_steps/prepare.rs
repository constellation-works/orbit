//! Select the escalated holds an escalation triage run reads (held pilot
//! assessments, or block episodes awaiting triage), draw each one's
//! hard-tier crew and partition them by crew.

use std::cell::OnceCell;
use std::collections::BTreeSet;

use chrono::{SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{Task, TaskComplexity, TaskStatus};
use orbit_types::workflow::automation::members::StateTriggerKind;
use serde_json::{Value, json};

use super::{HeldPilotWarnings, PilotWarningCandidacy};
use crate::OrbitRuntime;
use crate::application::task::TriageCandidacy;
use crate::application::task::batch_prepare::{
    BatchSpec, Qualification, held_by_run_detail, prepare_batch,
};
use crate::application::task::pilot::input::requested_workspace_root;
use crate::application::task::pilot::requested_base_branch;
use crate::application::task::pilot::source::{SourceSnapshot, resolve_source_snapshot};
use crate::application::task::{TaskAutomationPort, TaskDistributedPort, TaskJobPort};
use crate::application::task::{TaskListFilter, VerifierAssignment};

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
    let now = OnceCell::new();
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
            silent_reasons: &["not_held", "not_blocked"],
            before_workspace: vec![("subject", json!(subject.as_str()))],
            before_task_count: Vec::new(),
        },
        |runtime| runtime.escalation_triage_holders(),
        |_| true,
        |task, held, _explicit| {
            if held.contains_key(&task.id) {
                return Ok(Qualification::Rejected("already_triaging"));
            }
            let now = *now.get_or_init(Utc::now);
            match qualify(runtime, subject, task, now)? {
                Ok((row, author)) => Ok(Qualification::Selected((row, author))),
                Err(reason) => Ok(Qualification::Rejected(reason)),
            }
        },
        |task, (mut row, author): (Value, Option<String>)| {
            let triager = triager(runtime, task, author.as_deref())?;
            let crew = triager.crew.clone();
            row["crew"] = json!(crew);
            row["tier"] = json!(triager.tier);
            Ok((row, crew))
        },
        skipped_detail,
        |_, _| None,
    )
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
        .stored_run_state(run_id)?
        .as_ref()
        .and_then(|state| runtime.pilot_assessment_crew(state)))
}

/// `task`'s triager: a hard-tier draw that can inspect source natively,
/// passing over the assessment's author while the pool offers another, else
/// the inspection-capable default crew, exactly as a finding verifier is
/// drawn. With neither, preparation fails with `inspection_crew_unavailable`
/// before any triager launches.
fn triager(
    runtime: &OrbitRuntime,
    task: &Task,
    author: Option<&str>,
) -> Result<VerifierAssignment, OrbitError> {
    let avoid = author.into_iter().collect::<Vec<_>>();
    if let Some(assignment) = runtime.draw_finding_verifier(task, TRIAGE_TIER, &avoid)? {
        return Ok(assignment);
    }
    let crew = runtime.inspection_default_crew(TRIAGE_TIER)?;
    Ok(VerifierAssignment {
        crew: crew.name,
        tier: TRIAGE_TIER,
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
