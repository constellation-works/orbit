use orbit_types::workflow::{FinalRecoveryDecision, FinalRecoveryResume};

use crate::context::{
    CompletionObservationAdmission, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest,
    FinalRecoveryApplication,
};
use crate::executor::automation::vcs::absorbed::verify_absorbed_candidate;
use crate::executor::automation::vcs::git::git_output;

use super::super::*;
use super::{decision, input, settlement, types};

/// How long the authenticated completion retry waits before it reads the
/// pull request again [ORB-17140].
const COMPLETION_OBSERVATION_RETRY_BACKOFF: std::time::Duration =
    std::time::Duration::from_secs(10);

/// Run the job's final recovery for the failure of top-level step
/// `step_index`, at most once per run. `budget` is the executor's own guard;
/// it is spent as soon as the host is asked, whatever it answers.
pub(in crate::activity_job::job_executor) fn attempt_final_recovery(
    job: &JobV2,
    ctx: &ExecCtx<'_>,
    step_index: usize,
    error: &DispatchError,
    budget: &mut types::FinalRecoveryBudget,
) -> types::FinalRecoveryVerdict {
    let error_text = error.to_string();
    let error_message = error_text.as_str();
    let Some(activity) = ctx.final_recovery_activity.as_ref() else {
        return types::FinalRecoveryVerdict::Skipped;
    };
    let step = &job.steps[step_index];
    let skip = |reason: &str| {
        settlement::emit_final_recovery_event(ctx, step, activity, "skipped", None, Some(reason));
        types::FinalRecoveryVerdict::Skipped
    };
    if matches!(budget, types::FinalRecoveryBudget::Spent) {
        return skip("final recovery already ran for this run");
    }
    // A normal base race is owned by the deterministic preparation boundary.
    // Its refusal/exhaustion preserves the candidate; an agent cannot renew
    // its host authority or replenish its bounded attempts and deadline.
    if step.id == "sync_base" && error_message.contains("prepared_base_refresh_refused:") {
        return skip(
            "host-owned preparation recovery refused or exhausted; preserve the candidate without source repair",
        );
    }
    // [ORB-19224] No decision about the task lets a lane run a required check
    // its sandbox refuses, such as `orbit plugin test` inside a macOS worker
    // that did not hand it to the host replay: the failure handoff keeps the
    // candidate and a claimed leaf releases its claim.
    if orbit_types::workflow::is_validation_route_unavailable(None, Some(error_message)) {
        return skip(
            "required validation cannot run in the lane that tried it and was not handed to its \
             host replay; the candidate was not judged",
        );
    }
    // [ORB-13987] No decision about the task fixes a host that lacks a tool;
    // the failure handoff keeps the candidate for a resume instead.
    if orbit_types::workflow::is_validation_environment_failure(None, Some(error_message)) {
        return skip(
            "required validation lacked a tool in its environment; the candidate was not judged",
        );
    }
    // [ORB-14258] Nor fix a required command the base fails the same way; the
    // failure handoff holds the task until the command passes on a new base.
    if orbit_types::workflow::is_baseline_red_failure(None, Some(error_message)) {
        return skip(
            "required validation fails on the base exactly as on the candidate; the candidate \
             did not cause it",
        );
    }
    // [ORB-14269] Nor does another agent clear a blocker the implementer or
    // step recovery [ORB-14268] declared. The failure handoff records the
    // kind and keeps the candidate.
    if orbit_types::workflow::is_task_blocked_by_agent(None, Some(error_message)) {
        return skip("an agent declared a blocker; the candidate was not judged by another agent");
    }
    // Nor judges a candidate whose agent an Orbit upgrade refused mid-step;
    // the failure handoff keeps it for the run after the upgrade.
    if orbit_types::workflow::is_upgrade_pending(None, Some(error_message)) {
        return skip("an Orbit upgrade refused the step's agent; the candidate was not judged");
    }
    // [ORB-14149] Nor does any decision give the provider's model capacity;
    // the candidate stays in the worktree for a resume.
    if orbit_types::workflow::is_provider_capacity_exhausted(None, Some(error_message)) {
        return skip(
            "the provider reported the selected model at capacity; the candidate was not judged",
        );
    }
    // [ORB-14695] Nor outlasts the provider account's usage limit; run
    // finalization holds the task until the reset the provider reported.
    if orbit_types::workflow::is_provider_limit(None, Some(error_message)) {
        return skip(
            "the provider reported its account's usage limit reached; the candidate was not judged",
        );
    }
    // [ORB-14266] Nor makes an unusable provider usable, or gets past its
    // content policy. Run finalization holds the task in the backlog with
    // that provider's crews excluded for a while instead.
    if orbit_types::workflow::is_provider_unavailable(None, Some(error_message)) {
        return skip("the provider could not be used on this host; the candidate was not judged");
    }
    if orbit_types::workflow::is_provider_refusal(None, Some(error_message)) {
        return skip(
            "the provider's content policy refused the turn; the candidate was not judged",
        );
    }
    // [ORB-14260] Nor can a recovery agent in the same sandbox reach the
    // claimed task's owner its step could not; the claim is released.
    if orbit_types::workflow::is_owner_route_unavailable(None, Some(error_message)) {
        return skip(
            "the claimed worker could not reach its owner through the run's coordinator; the \
             candidate was not judged",
        );
    }
    // A `worktree_setup` that could not find a dependency is not transient:
    // the dependency is either resolvable through its registered owner or
    // it is not, and re-running unchanged cannot change that answer. No
    // recovery agent requeues it.
    if is_worktree_setup_dependency_not_found(&step.id, error) {
        return skip(
            "worktree_setup could not resolve a dependency (task not found); \
             re-running unchanged cannot resolve it",
        );
    }
    let Some(task_id) = input::single_task_id(&ctx.input) else {
        return skip(
            "final recovery decides for exactly one task; this run carries none or several",
        );
    };
    let Some(worktree) = input::run_worktree(job, ctx, step_index) else {
        return skip("the run has no assigned worktree for final recovery to inspect");
    };

    if let types::FinalRecoveryBudget::Replay(decision) =
        std::mem::replace(budget, types::FinalRecoveryBudget::Spent)
    {
        let application = FinalRecoveryApplication {
            task_id,
            failed_step_id: step.id.clone(),
            decision,
            resume_step_index: None,
            preserved_steps: Vec::new(),
            repair_commit: None,
            resume_context: None,
            workspace_path: worktree.workspace_path.into(),
            completion_done: ctx.input.get("completion").and_then(Value::as_str) == Some("done"),
            infrastructure: None,
        };
        return settlement::conclude(job, ctx, step, activity, &application, None, None);
    }
    // [ORB-14668] A candidate conflict recovery found already on the pinned
    // base is settled from the host-certified evidence, re-observed here,
    // without dispatching an agent. A refusal leaves the agent its look, and
    // tells it why.
    let absorbed = matches!(error, DispatchError::CandidateAbsorbed { .. }).then(|| {
        verify_absorbed_candidate(
            ctx.host,
            &ctx.run_id,
            &step.id,
            &task_id,
            std::path::Path::new(&worktree.workspace_path),
        )
    });
    let request = FinalRecoveryAdmissionRequest {
        task_id: task_id.clone(),
        failed_step_id: step.id.clone(),
        base_ref: worktree.base_ref.clone(),
    };
    match ctx.host.admit_final_recovery(&ctx.run_id, &request) {
        Ok(FinalRecoveryAdmission::Admitted) => {}
        Ok(FinalRecoveryAdmission::Skipped { reason }) => return skip(&reason),
        Err(error) => return skip(&format!("admission failed: {error}")),
    }
    let absorbed_refusal = match absorbed {
        Some(Ok(candidate)) => {
            return settlement::settle_absorbed(
                job, ctx, step, activity, task_id, worktree, &candidate,
            );
        }
        Some(Err(refusal)) => {
            tracing::warn!(
                target: "orbit.engine.job_executor",
                run_id = %ctx.run_id,
                failed_step_id = %step.id,
                reason = refusal.reason.code(),
                detail = %refusal.detail,
                "absorbed candidate not settled without an agent"
            );
            Some(refusal)
        }
        None => None,
    };

    // A deterministic delivery step that lost only to a lock or a locked
    // store judged nothing about the candidate, and no agent can make the
    // holder let go. The host blocks the task on that condition; once the run
    // has stopped, a passing probe requeues it onto the same candidate
    // (`orbit task recheck-blocked --confirm` and the owner's clock tick).
    // A claimed leaf is left to its agent: its task is the owner's, which
    // settles the claim through its own recovery and never sees this class.
    if let DispatchError::DeterministicActionFailed { action, message } = error
        && ctx.input.get("claimed").and_then(Value::as_bool) != Some(true)
        && let Some(class) =
            orbit_types::workflow::TransientInfrastructure::of_action_failure(action, message)
    {
        let application = FinalRecoveryApplication {
            task_id,
            failed_step_id: step.id.clone(),
            decision: FinalRecoveryDecision::Escalate {
                diagnosis: format!(
                    "step `{}` failed on transient infrastructure ({}): {}; the candidate was not \
                     judged. Original error: {}",
                    step.id,
                    class.as_str(),
                    class.describe(),
                    bounded_recovery_text("error", &ctx.run_id, error_message, 4 * 1024),
                ),
                human_action: "Nothing, unless it recurs: once this run has stopped and the \
                               condition clears, `orbit task recheck-blocked --confirm` (and the \
                               clock tick) requeue the task onto the candidate its failure handoff \
                               published. After the run stops, `orbit task requeue` does it at \
                               once."
                    .into(),
            },
            resume_step_index: None,
            preserved_steps: Vec::new(),
            repair_commit: None,
            resume_context: None,
            workspace_path: worktree.workspace_path.into(),
            completion_done: ctx.input.get("completion").and_then(Value::as_str) == Some("done"),
            infrastructure: Some(class),
        };
        return settlement::conclude(job, ctx, step, activity, &application, None, None);
    }

    // Read-only completion transport failures spend the same durable final
    // recovery budget, but need neither an agent nor a replacement candidate.
    // The single retry waits out a short outage first; the cumulative
    // completion deadline keeps counting through the wait.
    if matches!(
        error,
        DispatchError::CompletionObservationUnavailable { .. }
    ) {
        let authentication =
            ctx.host
                .completion_observation_admission(&ctx.run_id, &task_id, &step.id);
        let refusal = match authentication {
            Ok(CompletionObservationAdmission::Retry) => {
                crate::executor::automation::vcs::fixed_wait(COMPLETION_OBSERVATION_RETRY_BACKOFF);
                let rationale =
                    "transient-observation: authenticated unchanged publication; retry current completion observation"
                        .to_string();
                let application = FinalRecoveryApplication {
                    task_id,
                    failed_step_id: step.id.clone(),
                    decision: FinalRecoveryDecision::Resume {
                        step_id: step.id.clone(),
                        rationale: rationale.clone(),
                    },
                    resume_step_index: Some(step_index as u32),
                    preserved_steps: Vec::new(),
                    repair_commit: None,
                    resume_context: Some(FinalRecoveryResume {
                        step_id: step.id.clone(),
                        failed_step_id: step.id.clone(),
                        rationale,
                        refusal: bounded_recovery_text(
                            "refusal",
                            &ctx.run_id,
                            error_message,
                            MAX_RECOVERY_ERROR_MESSAGE_BYTES,
                        ),
                        refused_output: Value::Null,
                    }),
                    workspace_path: worktree.workspace_path.into(),
                    completion_done: ctx.input.get("completion").and_then(Value::as_str)
                        == Some("done"),
                    infrastructure: None,
                };
                return settlement::conclude(
                    job,
                    ctx,
                    step,
                    activity,
                    &application,
                    Some(step_index),
                    None,
                );
            }
            Ok(CompletionObservationAdmission::PolicyChanged { reason }) => {
                // The ordinary bounded requeue preserves the publication and
                // admits a fresh run under current policy; no old gate is reused.
                let application = FinalRecoveryApplication {
                    task_id,
                    failed_step_id: step.id.clone(),
                    decision: FinalRecoveryDecision::Requeue {
                        reason: format!(
                            "policy-changed: {reason}; transient completion observation requires current-policy source repair"
                        ),
                    },
                    resume_step_index: None,
                    preserved_steps: Vec::new(),
                    repair_commit: None,
                    resume_context: None,
                    workspace_path: worktree.workspace_path.into(),
                    completion_done: ctx.input.get("completion").and_then(Value::as_str)
                        == Some("done"),
                    infrastructure: None,
                };
                return settlement::conclude(job, ctx, step, activity, &application, None, None);
            }
            Ok(CompletionObservationAdmission::Unavailable) => {
                "host has no authenticated completion prefix".to_string()
            }
            Err(error) => error.to_string(),
        };
        // A refusal must not reach an agent that can request the same stale
        // prefix again. Preserve the publication for explicit governed repair.
        let application = FinalRecoveryApplication {
            task_id,
            failed_step_id: step.id.clone(),
            decision: FinalRecoveryDecision::Escalate {
                diagnosis: format!("unchanged completion reuse refused: candidate-changed: {refusal}; original error: {error_message}"),
                human_action: "Inspect the publication and admit current-policy source repair when its evidence or inputs changed.".into(),
            },
            resume_step_index: None,
            preserved_steps: Vec::new(),
            repair_commit: None,
            resume_context: None,
            workspace_path: worktree.workspace_path.into(),
            completion_done: ctx.input.get("completion").and_then(Value::as_str) == Some("done"),
            infrastructure: None,
        };
        return settlement::conclude(job, ctx, step, activity, &application, None, None);
    }

    let recovery_input =
        match input::final_recovery_input(job, ctx, step_index, &task_id, &worktree, error_message)
        {
            Ok(input::FinalRecoveryInput::Wait(reason)) => {
                // Completion spent its watch budget. Preserve the publication and
                // use the existing bounded requeue, rather than starting another
                // watch or asking an agent to diagnose unconcluded checks.
                let application = FinalRecoveryApplication {
                    task_id,
                    failed_step_id: step.id.clone(),
                    decision: FinalRecoveryDecision::Requeue { reason },
                    resume_step_index: None,
                    preserved_steps: Vec::new(),
                    repair_commit: None,
                    resume_context: None,
                    workspace_path: worktree.workspace_path.into(),
                    completion_done: ctx.input.get("completion").and_then(Value::as_str)
                        == Some("done"),
                    infrastructure: None,
                };
                return settlement::conclude(job, ctx, step, activity, &application, None, None);
            }
            Ok(input::FinalRecoveryInput::Ready(input)) => Ok(input),
            Err(error) => Err(error),
        };

    // These observations belong to the engine, not the activity's response.
    // A worktree without a resolvable HEAD may still settle a task, but cannot
    // authorize a moved HEAD in a later commit step.
    let head_before = git_output(
        std::path::Path::new(&worktree.workspace_path),
        &["rev-parse", "--verify", "HEAD^{commit}"],
    )
    .ok();
    let root_before = std::path::Path::new(&worktree.workspace_path)
        .canonicalize()
        .ok();
    let branch_before = git_output(
        std::path::Path::new(&worktree.workspace_path),
        &["symbolic-ref", "--quiet", "HEAD"],
    )
    .ok();
    let dispatch = recovery_input
        .map(|mut input| {
            if let Some(refusal) = &absorbed_refusal {
                input["absorbed_refusal"] = refusal.evidence();
            }
            input
        })
        .and_then(|input| decision::dispatch_final_recovery(ctx, step, activity, &input));
    let (decision, recovery_model) = match dispatch {
        Ok((output, model)) => (
            FinalRecoveryDecision::from_output(Some(&decision::decision_payload(&output))),
            model,
        ),
        Err(message) => (
            FinalRecoveryDecision::Escalate {
                diagnosis: format!(
                    "final recovery activity `{}` did not return a decision: {message}",
                    activity.name
                ),
                human_action:
                    "Inspect the failed run and its final-recovery invocation, then move \
                               the task by hand."
                        .to_string(),
            },
            None,
        ),
    };
    let (mut decision, mut resume_index, downgrade) =
        decision::admit_resume_step(job, step_index, decision);
    let commit_index = job.steps[1..=step_index]
        .iter()
        .position(|step| {
            matches!(&step.body, JobV2StepBody::Target(target)
            if matches!(&target.spec, ActivityV2Spec::Deterministic(spec)
                if spec.action == "git_commit"))
        })
        .map(|index| index + 1);
    if resume_index.is_some()
        && let Some(index) = commit_index
        && let Err(refusal) = recovery::commit_final_recovery_repair(
            ctx,
            step,
            activity,
            &job.steps[index],
            &task_id,
            &worktree,
            root_before
                .as_deref()
                .zip(head_before.as_deref())
                .zip(branch_before.as_deref())
                .map(|((root, head), branch)| (root, head, branch)),
            recovery_model.as_deref(),
        )
    {
        decision = FinalRecoveryDecision::Escalate {
            diagnosis: format!("{refusal}; original error before recovery: {error_message}"),
            human_action:
                "Inspect the refused repair and its candidate ownership before resuming the run."
                    .into(),
        };
        resume_index = None;
    }
    let repair_commit = resume_index.and_then(|_| {
        settlement::observe_repair_commit(&worktree.workspace_path, head_before.as_deref())
    });
    if repair_commit.is_some()
        && let Some(commit_index) = commit_index
    {
        // A repair is a new candidate. Even a request to resume settlement
        // must rerun commit, validation, admission and review first.
        let index = resume_index.unwrap_or(step_index).min(commit_index);
        if let FinalRecoveryDecision::Resume { step_id, .. } = &mut decision {
            *step_id = job.steps[index].id.clone();
        }
        resume_index = Some(index);
    }

    // The context names the admitted step, not the decision's text, which
    // admission matched after trimming. The resumed agent gets the same bounded
    // copies final recovery read: an unbounded diagnostic would make the
    // provider refuse the resumed turn before it starts [ORB-12467]. The full
    // error stays in the step record.
    let resume_context = match (&decision, resume_index) {
        (FinalRecoveryDecision::Resume { rationale, .. }, Some(index)) => {
            Some(FinalRecoveryResume {
                step_id: job.steps[index].id.clone(),
                failed_step_id: step.id.clone(),
                rationale: rationale.clone(),
                refusal: bounded_recovery_text(
                    "refusal",
                    &ctx.run_id,
                    error_message,
                    MAX_RECOVERY_ERROR_MESSAGE_BYTES,
                ),
                refused_output: bounded_recovery_input(
                    &ctx.run_id,
                    input::refused_step_output(ctx, step),
                ),
            })
        }
        _ => None,
    };
    let preserved_steps = if resume_index.is_some() && repair_commit.is_none() {
        decision::preserved_publication_steps(job, ctx, &task_id, &worktree)
    } else {
        Vec::new()
    };
    let application = FinalRecoveryApplication {
        task_id,
        failed_step_id: step.id.clone(),
        decision: decision.clone(),
        resume_step_index: resume_index.map(|index| index as u32),
        preserved_steps,
        repair_commit,
        resume_context,
        workspace_path: worktree.workspace_path.into(),
        completion_done: ctx.input.get("completion").and_then(Value::as_str) == Some("done"),
        infrastructure: None,
    };
    settlement::conclude(
        job,
        ctx,
        step,
        activity,
        &application,
        resume_index,
        downgrade.as_deref(),
    )
}

/// Whether `error` is a `worktree_setup` failure caused by a dependency
/// lookup that found no task. Only the step's own deterministic failure
/// counts, and only when it carries the remote `not_found` / `task not
/// found` text the owner lookup produces.
fn is_worktree_setup_dependency_not_found(step_id: &str, error: &DispatchError) -> bool {
    if step_id != "worktree_setup" {
        return false;
    }
    let message = error.to_string();
    message.contains("task not found")
        && (message.contains("not_found") || message.contains("NotFound"))
}
