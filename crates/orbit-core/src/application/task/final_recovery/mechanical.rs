//! The mechanical recoveries final recovery applies itself before it asks a
//! human anything:
//!
//! - **resume_run**: resume the blocking run from its checkpoints through
//!   [`OrbitRuntime::submit_resume_run`], the path `orbit job resume` takes.
//!   At most once per run.
//! - **rerun_ci**: re-request the checks that failed on the task's pull
//!   request at the head the agent inspected, through the runtime's
//!   [`TaskPrForge`](crate::runtime::task_pr_forge::TaskPrForge), then requeue
//!   the task so its next run continues the publication. At most once per
//!   head. A failure the base-red path already owns is left to it.
//! - **friction_reported**: link an existing friction record, or file one
//!   during the task, and escalate with the exact human action.
//!
//! Resumes and CI-rerun requeues count in the [`FinalRecoveryRequeueBound`]
//! every requeue shares. Every precondition is checked before the recovery
//! acts. A refused step changes nothing and escalates with the refusal as its
//! reason, so the card says why the automatic step did not run.

use chrono::Utc;
use orbit_common::governance::friction::normalize_title;
use orbit_store::contracts::FrictionAddParams;
use orbit_types::record::FrictionStatus;
use orbit_types::task::{Task, TaskHistoryEntry};
use serde_json::Value;

use super::super::blocked_recovery::{BLOCKED_TASK_TRIAGE_RESUMED_EVENT, blocking_run_of};
use super::super::helpers::SYSTEM_ACTOR_LABEL;
use super::apply::Plan;
use super::request::{FinalRecoveryOutcome, FinalRecoveryRequest, FinalRecoveryRequeueBound};
use super::{
    FINAL_RECOVERY_CI_RERUN_EVENT, FINAL_RECOVERY_REQUEUED_EVENT, FINAL_RECOVERY_RESUMED_EVENT,
};
use crate::OrbitRuntime;

/// History events whose `failed_head` or `head` note field marks a head's
/// failure as inherited from the base, which the base-red path owns.
const BASE_RED_EVENTS: [(&str, &str); 2] = [
    ("pr_auto_fix_base_red", "failed_head"),
    ("ci_base_suspect", "head"),
];

/// Tags whose tasks only an operator requeues.
const OPERATOR_ONLY_TAGS: [&str; 2] = ["security", "security-review"];

/// Why only an operator may requeue `task`, when only one may: a security
/// tag, or a run another machine executed whose evidence this owner cannot
/// read.
pub(in crate::application::task) fn operator_only_requeue(
    runtime: &OrbitRuntime,
    task: &Task,
) -> Option<String> {
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

/// What a step produced: its plan, or why it did not run.
type Step = Result<Plan, String>;

impl OrbitRuntime {
    /// The shared requeue bound's count before one more requeue or resume,
    /// refused when unreadable (fail closed) or exhausted.
    pub(in crate::application::task) fn mechanical_bound(
        &self,
        task: &Task,
        step: &str,
    ) -> Result<(usize, FinalRecoveryRequeueBound), String> {
        let bound = request_bound();
        let recent = self
            .recent_requeues(&task.id, bound.window)
            .map_err(|error| {
                format!("{step} refused: the shared requeue count cannot be read ({error})")
            })?;
        if recent >= bound.max_requeues {
            return Err(format!(
                "{step} refused: the task was already requeued or resumed {recent} time(s) in the \
                 last {} hour(s) (limit {} per {} hour(s), shared with every requeue)",
                bound.window.num_hours(),
                bound.max_requeues,
                bound.window.num_hours()
            ));
        }
        Ok((recent, bound))
    }

    /// `resume_run`: resume the blocking run once, readmitting the task.
    pub(super) fn plan_resume_run(
        &self,
        task: &Task,
        request: &FinalRecoveryRequest,
        header: &str,
        run_id: &str,
        reason: &str,
    ) -> Plan {
        let attempt = || -> Step {
            const STEP: &str = "resume_run";
            let history = self.get_task_history(&task.id).map_err(|e| e.to_string())?;
            let blocking = blocking_run_of(task, &history);
            if blocking.as_deref() != Some(run_id) && request.run_id != run_id {
                return Err(format!(
                    "{STEP} refused: run {run_id} is not the run that blocked the task ({})",
                    blocking.as_deref().unwrap_or("none recorded")
                ));
            }
            if let Some(reason) = operator_only_requeue(self, task) {
                return Err(format!("{STEP} refused: {reason}"));
            }
            let marker = format!("source_run_id={run_id}");
            if history.iter().any(|entry| {
                matches!(
                    entry.event.as_str(),
                    FINAL_RECOVERY_RESUMED_EVENT | BLOCKED_TASK_TRIAGE_RESUMED_EVENT
                ) && entry
                    .note
                    .as_deref()
                    .is_some_and(|note| note.split_whitespace().any(|field| field == marker))
            }) {
                return Err(format!(
                    "{STEP} refused: run {run_id} was already resumed once; a second resume \
                     repeats the same failure"
                ));
            }
            let (recent, bound) = self.mechanical_bound(task, STEP)?;
            // The resume path validates resumability before it inserts a run or
            // readmits the task, so its refusal changes nothing either.
            let invoke = self
                .submit_resume_run(run_id, Some("final-recovery"), None)
                .map_err(|error| format!("{STEP} refused: {error}"))?;
            Ok(Plan {
                status: task.status,
                event: FINAL_RECOVERY_RESUMED_EVENT,
                note: String::new(),
                comment: format!(
                    "{header} outcome=requeued requeue={} of {} per {} hour(s)\n\
                     resumed_run_id: {}\nreason: {}",
                    recent + 1,
                    bound.max_requeues,
                    bound.window.num_hours(),
                    invoke.run_id,
                    reason.trim()
                ),
                execution_summary: None,
                outcome: FinalRecoveryOutcome::Requeued,
                infra: None,
                readmitted: true,
                history: vec![system_entry(
                    FINAL_RECOVERY_RESUMED_EVENT,
                    format!("source_run_id={run_id} resumed_run_id={}", invoke.run_id),
                )],
                raise: None,
            })
        };
        attempt().unwrap_or_else(|refusal| refused_step(request, header, "resume_run", refusal))
    }

    /// `rerun_ci`: re-request the failed required checks at `head_sha`, once
    /// per head, and requeue the task.
    pub(super) fn plan_rerun_ci(
        &self,
        task: &Task,
        request: &FinalRecoveryRequest,
        header: &str,
        head_sha: &str,
        reason: &str,
    ) -> Plan {
        let attempt = || -> Step {
            const STEP: &str = "rerun_ci";
            if let Some(reason) = operator_only_requeue(self, task) {
                return Err(format!("{STEP} refused: {reason}"));
            }
            let history = self.get_task_history(&task.id).map_err(|e| e.to_string())?;
            let marker = format!("head={head_sha}");
            if history.iter().any(|entry| {
                entry.event == FINAL_RECOVERY_CI_RERUN_EVENT
                    && entry
                        .note
                        .as_deref()
                        .is_some_and(|note| note.split_whitespace().any(|field| field == marker))
            }) {
                return Err(format!(
                    "{STEP} refused: the failed checks of head {head_sha} were already re-run \
                     once"
                ));
            }
            let (recent, bound) = self.mechanical_bound(task, STEP)?;
            let number = task
                .github_pr_number()
                .and_then(|number| number.parse::<u64>().ok())
                .ok_or_else(|| format!("{STEP} refused: the task records no pull request"))?;
            let repo_root = &self.paths().repo_root;
            let forge = self.task_pr_forge();
            let blockers = forge
                .pull_request_blockers(repo_root, number)
                .map_err(|error| {
                    format!(
                        "{STEP} refused: the checks of pull request #{number} cannot be read \
                         ({error})"
                    )
                })?
                .ok_or_else(|| {
                    format!(
                        "{STEP} refused: pull request #{number} is not open, or the forge cannot \
                         report it"
                    )
                })?;
            if blockers.head_sha != head_sha {
                return Err(format!(
                    "{STEP} refused: pull request #{number} moved to head {}, not the inspected \
                     head {head_sha}",
                    blockers.head_sha
                ));
            }
            if blockers.failed_required_checks.is_empty() {
                return Err(format!(
                    "{STEP} refused: no required check failed on head {head_sha} of #{number}"
                ));
            }
            if let Some(event) = base_red_owner(&history, head_sha) {
                return Err(format!(
                    "{STEP} refused: `{event}` records head {head_sha}'s failure as inherited \
                     from the base, which the base-red path resolves"
                ));
            }
            let rerun = forge
                .rerun_failed_checks(repo_root, number, head_sha)
                .map_err(|error| format!("{STEP} refused: {error}"))?;
            let failed = blockers.failed_required_checks.join(", ");
            let requested = rerun
                .requested
                .iter()
                .map(|(id, name)| format!("{name} (run {id})"))
                .collect::<Vec<_>>()
                .join(", ");
            let detail = match &rerun.uncertain {
                Some((id, name, error)) => format!(
                    "request outcome uncertain: {name} (run {id}): {error}; inspect the workflow \
                     before retrying"
                ),
                None => "all requests acknowledged".to_string(),
            };
            Ok(Plan {
                status: orbit_types::task::TaskStatus::Backlog,
                event: FINAL_RECOVERY_REQUEUED_EVENT,
                note: format!(
                    "final recovery (run_id={}): requested reruns for failed checks ({failed}) on \
                     head {head_sha} of pull request #{number}",
                    request.run_id
                ),
                comment: format!(
                    "{header} outcome=requeued requeue={} of {} per {} hour(s)\n\
                     repair: requested reruns for failed checks ({failed}) on head {head_sha} of \
                     pull request #{number}; {}\nacknowledged: {requested}\n{detail}",
                    recent + 1,
                    bound.max_requeues,
                    bound.window.num_hours(),
                    reason.trim()
                ),
                execution_summary: None,
                outcome: FinalRecoveryOutcome::Requeued,
                infra: None,
                readmitted: false,
                history: vec![system_entry(
                    FINAL_RECOVERY_CI_RERUN_EVENT,
                    format!(
                        "head={head_sha} pr={number} requested={}",
                        rerun.requested.len()
                    ),
                )],
                raise: None,
            })
        };
        attempt().unwrap_or_else(|refusal| refused_step(request, header, "rerun_ci", refusal))
    }

    /// `friction_reported`: link or file the friction, then block the task
    /// for the exact human action.
    pub(super) fn plan_friction_reported(
        &self,
        task: &Task,
        request: &FinalRecoveryRequest,
        header: &str,
        friction: FrictionRef<'_>,
        human_action: &str,
        summary: Option<&str>,
    ) -> Plan {
        let attempt = || -> Step {
            const STEP: &str = "friction_reported";
            let store = crate::runtime::friction::store_for(self)
                .map_err(|error| format!("{STEP} refused: {error}"))?;
            let (record, filed) = match friction {
                FrictionRef::Existing(id) => (
                    store
                        .show(id)
                        .map_err(|error| format!("{STEP} refused: {error}"))?
                        .ok_or_else(|| format!("{STEP} refused: no friction record {id} exists"))?
                        .record,
                    false,
                ),
                FrictionRef::File { title, body } => (
                    store
                        .add(FrictionAddParams {
                            model: "final-recovery".to_string(),
                            title: Some(
                                normalize_title(title)
                                    .map_err(|error| format!("{STEP} refused: {error}"))?,
                            ),
                            body: body.to_string(),
                            tags: Vec::new(),
                            status: FrictionStatus::Open,
                            during_task: Some(task.id.clone()),
                            created_at: Utc::now(),
                        })
                        .map_err(|error| format!("{STEP} refused: {error}"))?
                        .record,
                    true,
                ),
            };
            let title = record.title.as_deref().unwrap_or("untitled");
            let diagnosis = format!(
                "Orbit defect recorded as friction {} ({title}){}",
                record.id,
                if filed {
                    ", filed by final recovery"
                } else {
                    ""
                }
            );
            let summary = summary
                .map(str::to_string)
                .unwrap_or_else(|| diagnosis.clone());
            Ok(Plan::escalated(
                request,
                header,
                &diagnosis,
                &format!(
                    "friction: {}\nhuman_action: {}",
                    record.id,
                    human_action.trim()
                ),
                None,
            )
            .with_human_request(&summary, human_action))
        };
        attempt()
            .unwrap_or_else(|refusal| refused_step(request, header, "friction_reported", refusal))
    }
}

/// The friction a `friction_reported` decision names.
pub(super) enum FrictionRef<'a> {
    /// An existing record, by id.
    Existing(&'a str),
    /// A new record, filed during the task.
    File { title: &'a str, body: &'a str },
}

/// The escalation a refused step records: nothing else changed.
fn refused_step(request: &FinalRecoveryRequest, header: &str, step: &str, refusal: String) -> Plan {
    let human_action = format!(
        "The automatic `{step}` did not apply. Read the reason, then requeue the task or move it \
         by hand."
    );
    Plan::escalated(
        request,
        header,
        &refusal,
        &format!("human_action: {human_action}"),
        Some(refusal.clone()),
    )
    .with_human_request(&refusal, &human_action)
}

fn request_bound() -> FinalRecoveryRequeueBound {
    FinalRecoveryRequeueBound::default()
}

/// A system history entry with `note`, stamped now.
fn system_entry(event: &str, note: String) -> TaskHistoryEntry {
    TaskHistoryEntry {
        at: Utc::now(),
        by: SYSTEM_ACTOR_LABEL.to_string(),
        event: event.to_string(),
        note: Some(note),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
    }
}

/// Whether `history` records any head's failure as inherited from the base.
pub(in crate::application::task) fn records_base_red(history: &[TaskHistoryEntry]) -> bool {
    history.iter().any(|entry| {
        BASE_RED_EVENTS
            .iter()
            .any(|(event, _)| entry.event == *event)
    })
}

/// The base-red event in `history` that records `head`'s failure as
/// inherited from the base, if any.
fn base_red_owner(history: &[TaskHistoryEntry], head: &str) -> Option<&'static str> {
    BASE_RED_EVENTS.iter().find_map(|(event, field)| {
        history
            .iter()
            .any(|entry| {
                entry.event == *event
                    && entry
                        .note
                        .as_deref()
                        .and_then(|note| serde_json::from_str::<Value>(note).ok())
                        .is_some_and(|note| note[*field] == head)
            })
            .then_some(*event)
    })
}
