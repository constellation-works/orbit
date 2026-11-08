//! Resolve reviewer identity and enforce the captured review budget.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::identity::Crew;
use orbit_types::workflow::{
    ReviewAdmission, ReviewGateAdmissionOutput, ReviewGateReviewer, ReviewerIdentity,
};
use serde_json::Value;

use super::super::context::GateContext;
use crate::OrbitRuntime;
use crate::application::contracts::AutoEffortSubject;
use crate::application::job::crew_pools::seeded_crew_ticket;
use crate::application::review::ReviewCandidateSizePort;
use crate::application::review::crew::{pool_source, pre_landing_review_crew_unset};
use crate::runtime::engine::crew::enforce_crew_allowlist;

/// The reviewer crew must be explicitly configured, resolvable on this host,
/// and inside the run's crew allowlist. It is never inferred from the
/// implementer; refusal escalates rather than substituting.
///
/// A captured pool chooses one member per candidate [ORB-15195], preferring
/// one that did not implement the run's tasks; the draw is seeded by the run
/// and candidate head, so a resumed admission chooses the same reviewer.
/// Returns the crew with the provenance its reviewer record carries.
pub(super) fn resolve_reviewer_crew(
    runtime: &OrbitRuntime,
    admission: &ReviewAdmission,
    context: &GateContext,
    base: &str,
    head: &str,
) -> Result<ReviewGateReviewer, OrbitError> {
    let run = runtime.get_job_run_backend(&context.run_id)?;
    let run_input = run.and_then(|run| run.input).unwrap_or(Value::Null);
    let allowlist = runtime.crew_allowlist_from_input(&run_input)?;
    if !admission.crew_pool.is_empty() {
        let names = admission
            .crew_pool
            .iter()
            .map(|member| member.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let candidates = runtime
            .review_crew_candidates(&admission.crew_pool)
            .map_err(|error| {
                OrbitError::CapabilityDenied(format!(
                    "review_crew_unavailable: no crew of the configured review pool [{names}] \
                     can be resolved on this host: {error}"
                ))
            })?;
        // A `<crew>@auto` member runs at the effort the reviewable diff and
        // the tasks' settled complexity choose.
        let candidates = if candidates
            .iter()
            .any(|candidate| candidate.auto_effort.is_some())
        {
            let subject = review_auto_effort_subject(runtime, context, base, head);
            runtime.reresolve_auto_candidates(candidates, &subject)?
        } else {
            candidates
        };
        let mut excluded = None;
        let permitted = candidates
            .into_iter()
            .filter(|candidate| {
                enforce_crew_allowlist(allowlist.as_ref(), &candidate.crew, "the review pool")
                    .map_err(|error| excluded.get_or_insert(error))
                    .is_ok()
            })
            .collect::<Vec<_>>();
        if permitted.is_empty() {
            let error = excluded.map_or_else(String::new, |error| error.to_string());
            return Err(OrbitError::CapabilityDenied(format!(
                "review_crew_excluded: {error}; the run's window excludes every crew of the \
                 configured review pool [{names}], and the gate escalates rather than \
                 substituting another crew"
            )));
        }
        let implementers = context
            .tasks
            .iter()
            .filter_map(|task| task.crew.clone())
            .chain(
                run_input
                    .get("crew")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
            .collect();
        let draw = runtime.draw_review_crew(
            permitted,
            &implementers,
            &mut seeded_crew_ticket(format!("review:{}:{head}", context.run_id)),
        )?;
        let mut reviewer = admitted_reviewer(&draw.crew, pool_source(&admission.crew_source));
        reviewer.eligible_pool = draw.eligible_pool;
        if let Some(choice) = &draw.auto_effort {
            reviewer.crew_source = format!(
                "{}; {}",
                reviewer.crew_source,
                crate::application::contracts::auto_effort_note(choice)
            );
        }
        reviewer.auto_effort = draw.auto_effort;
        return Ok(reviewer);
    }
    let name = admission.crew.as_deref().ok_or_else(|| {
        OrbitError::CapabilityDenied(format!(
            "review_crew_unconfigured: before-PR review needs an explicitly configured reviewer, \
             but {}; automatic review never inherits the implementer's crew",
            pre_landing_review_crew_unset()
        ))
    })?;
    let crew = runtime
        .resolve_crew_for_task(Some(name), None)
        .map_err(|error| {
            OrbitError::CapabilityDenied(format!(
                "review_crew_unavailable: configured review crew '{name}' cannot be resolved on \
                 this host: {error}"
            ))
        })?;
    enforce_crew_allowlist(allowlist.as_ref(), &crew, "the configured review crew").map_err(
        |error| {
            OrbitError::CapabilityDenied(format!(
                "review_crew_excluded: {error}; the gate escalates rather than substituting a \
                 crew the run's window excluded"
            ))
        },
    )?;
    Ok(admitted_reviewer(&crew, admission.crew_source.clone()))
}

fn admitted_reviewer(crew: &Crew, source: String) -> ReviewGateReviewer {
    ReviewGateReviewer {
        crew: crew.name.clone(),
        crew_source: source,
        provider: crew.assignment.provider.clone(),
        model: crew.assignment.model.clone(),
        reasoning_effort: crew.assignment.effort.map(|effort| effort.to_string()),
        eligible_pool: Vec::new(),
        auto_effort: None,
    }
}

/// What a reviewer's `<crew>@auto` resolves against: the candidate's
/// reviewable lines and changed files, and the highest settled complexity of
/// the reviewed tasks. Seeded by the run and head, so a resumed admission
/// resolves alike.
fn review_auto_effort_subject(
    runtime: &OrbitRuntime,
    context: &GateContext,
    base: &str,
    head: &str,
) -> AutoEffortSubject {
    let size = runtime.review_candidate_size(&context.workspace_path, base, head);
    let complexity = context
        .tasks
        .iter()
        .filter_map(|task| task.complexity.filter(|tier| tier.is_assessed()))
        .max_by_key(|tier| {
            ["low", "medium", "hard", "xhard"]
                .iter()
                .position(|name| *name == tier.as_str())
        });
    AutoEffortSubject {
        signal: orbit_types::identity::AutoEffortSignal {
            complexity,
            lines: size.map(|(lines, _)| lines),
            files: size.map(|(_, files)| files),
        },
        seed: Some(format!("review-auto-effort:{}:{head}", context.run_id)),
        forced: None,
    }
}

/// The reviewer a held review settles under: the one that reviewed it.
pub(super) fn held_reviewer(reviewer: &ReviewerIdentity) -> ReviewGateReviewer {
    ReviewGateReviewer {
        crew: reviewer.crew.clone(),
        crew_source: "held_review".to_string(),
        provider: reviewer.provider.clone(),
        model: reviewer.model.clone(),
        reasoning_effort: reviewer.reasoning_effort.clone(),
        eligible_pool: Vec::new(),
        auto_effort: None,
    }
}

pub(in crate::application::review::gate) fn reviewer_identity(
    runtime: &OrbitRuntime,
    context: &GateContext,
    reviewer: &ReviewGateReviewer,
) -> Result<ReviewerIdentity, OrbitError> {
    let implementer_model = runtime
        .get_job_run_backend(&context.run_id)?
        .and_then(|run| run.crew_model)
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty());
    Ok(ReviewerIdentity {
        crew: reviewer.crew.clone(),
        provider: reviewer.provider.clone(),
        same_model_as_implementer: implementer_model.as_deref() == Some(reviewer.model.as_str()),
        model: reviewer.model.clone(),
        reasoning_effort: reviewer.reasoning_effort.clone(),
        implementer_model,
    })
}

/// Fail before implementation when the lineage's latest review ran out of
/// minutes without a verdict: a resumed run would otherwise implement again
/// only to be refused at admission. A settled review never blocks here — the
/// next candidate is a new review. This does not reserve an attempt, load Git
/// objects or write a manifest.
pub(super) fn preflight_budget(
    runtime: &OrbitRuntime,
    context: &GateContext,
) -> Result<ReviewGateAdmissionOutput, OrbitError> {
    let lineage = context.lineage_key();
    if let Some(ledger) = runtime
        .review_store()?
        .review_ledger(&context.workspace_id, &lineage)?
        && let Some(latest) = ledger.latest_attempt()
        && !ledger.reviewed(&latest.candidate, &latest.task_meaning_digest)
        && ledger.remaining_at(Utc::now()).seconds == 0
    {
        return Err(OrbitError::CapabilityDenied(format!(
            "review_budget_exhausted: review_minutes_exhausted for lineage '{lineage}'; {}",
            budget_recovery(context, &lineage)
        )));
    }
    Ok(ReviewGateAdmissionOutput::PreflightPassed {
        lineage_key: lineage,
    })
}

/// The operator's way past an exhausted budget. A claim is one attempt that
/// keys its own lineage and is never resumed, so its budget is not reset:
/// the owner requeues the task, and its next claim continues the candidate
/// with a budget of its own.
pub(super) fn budget_recovery(context: &GateContext, lineage: &str) -> String {
    let task_id = &context.task_ids[0];
    if context.claimed {
        format!(
            "the owner can run orbit task requeue {task_id} --reason '<decision>'; the task's \
             next claim continues this candidate with a review budget of its own"
        )
    } else {
        format!(
            "an operator can run orbit task review-reset {task_id} --lineage '{lineage}' \
             --reason '<decision>' before resuming, or dispatch a fresh delivery run"
        )
    }
}
