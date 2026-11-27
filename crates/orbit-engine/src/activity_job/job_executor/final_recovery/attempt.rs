use orbit_types::workflow::{
    FaultAttribution, FinalRecoveryDecision, FinalRecoveryRepairCommit, FinalRecoveryResume,
    RecoveryFault, SYNC_BASE_STEP_ID, WORKTREE_SETUP_STEP_ID,
};

use crate::context::{
    CompletionObservationAdmission, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest,
    FinalRecoveryApplication,
};
use crate::executor::automation::vcs::absorbed::{AbsorbedRefusal, verify_absorbed_candidate};
use crate::executor::automation::vcs::git::git_output;

use super::super::*;
use super::{decision, input, settlement, types};

/// How long the authenticated completion retry waits before it reads the
/// pull request again [ORB-17140].
const COMPLETION_OBSERVATION_RETRY_BACKOFF: std::time::Duration =
    std::time::Duration::from_secs(10);

/// A failure admitted for final recovery: what the phases below read.
struct Attempt<'a, 'c> {
    job: &'a JobV2,
    ctx: &'a ExecCtx<'c>,
    step_index: usize,
    step: &'a JobV2Step,
    activity: &'a ResolvedRecoveryActivity,
    error: &'a DispatchError,
    error_message: &'a str,
    task_id: String,
    worktree: types::RunWorktree,
    /// Why an absorbed candidate was not settled without an agent.
    absorbed_refusal: Option<AbsorbedRefusal>,
}

impl Attempt<'_, '_> {
    /// An application of `decision` that resumes nothing: the starting point
    /// every deterministic conclusion and the provider path build on.
    fn application(
        &self,
        decision: FinalRecoveryDecision,
        fault: FaultAttribution,
    ) -> FinalRecoveryApplication {
        FinalRecoveryApplication {
            task_id: self.task_id.clone(),
            failed_step_id: self.step.id.clone(),
            decision,
            fault,
            resume_step_index: None,
            preserved_steps: Vec::new(),
            repair_commit: None,
            resume_context: None,
            workspace_path: self.worktree.workspace_path.clone().into(),
            completion_done: self.ctx.input.get("completion").and_then(Value::as_str)
                == Some("done"),
            infrastructure: None,
        }
    }

    /// Hand a resume-free `application` to the host.
    fn conclude(&self, application: &FinalRecoveryApplication) -> types::FinalRecoveryVerdict {
        settlement::conclude(
            self.job,
            self.ctx,
            self.step,
            self.activity,
            application,
            None,
            None,
        )
    }

    fn claimed(&self) -> bool {
        self.ctx.input.get("claimed").and_then(Value::as_bool) == Some(true)
    }

    fn completion_observation_unavailable(&self) -> bool {
        matches!(
            self.error,
            DispatchError::CompletionObservationUnavailable { .. }
        )
    }
}

/// The checkout the recovery agent starts from.
///
/// These observations belong to the engine, not the activity's response. A
/// worktree without a resolvable HEAD may still settle a task, but cannot
/// authorize a moved HEAD in a later commit step.
struct CheckoutSnapshot {
    head: Option<String>,
    root: Option<std::path::PathBuf>,
    branch: Option<String>,
}

impl CheckoutSnapshot {
    fn observe(ctx: &ExecCtx<'_>, workspace_path: &str) -> Self {
        let workspace = std::path::Path::new(workspace_path);
        Self {
            head: git_output(
                &ctx.host.process_env(),
                workspace,
                &["rev-parse", "--verify", "HEAD^{commit}"],
            )
            .ok(),
            root: workspace.canonicalize().ok(),
            branch: git_output(
                &ctx.host.process_env(),
                workspace,
                &["symbolic-ref", "--quiet", "HEAD"],
            )
            .ok(),
        }
    }

    /// The root, HEAD and branch together, only when all three were observed.
    fn ownership(&self) -> Option<(&std::path::Path, &str, &str)> {
        self.root
            .as_deref()
            .zip(self.head.as_deref())
            .zip(self.branch.as_deref())
            .map(|((root, head), branch)| (root, head, branch))
    }
}

/// The provider's answer to the failure, before the engine admits any resume.
struct ProviderAnswer {
    decision: FinalRecoveryDecision,
    fault: FaultAttribution,
    /// The dispatched spec's resolved model [ORB-16768].
    model: Option<String>,
    /// A resume's evidence, held until its step and repair commit are admitted.
    repair_evidence: Option<Value>,
}

/// What the engine admitted of the provider's answer.
struct RecoveryOutcome {
    decision: FinalRecoveryDecision,
    fault: FaultAttribution,
    /// The admitted top-level step to resume from, when the decision resumes.
    resume_index: Option<usize>,
    downgrade: Option<String>,
    repair_commit: Option<FinalRecoveryRepairCommit>,
}

/// Run the job's final recovery for the failure of top-level step
/// `step_index`, at most once per run. `budget` is the executor's own guard;
/// it is spent as soon as the host is asked, whatever it answers.
pub(in crate::activity_job::job_executor) fn attempt_final_recovery(
    job: &JobV2,
    ctx: &ExecCtx<'_>,
    step_index: usize,
    error: &DispatchError,
    failure_code: Option<&orbit_types::workflow::StepFailureCode>,
    budget: &mut types::FinalRecoveryBudget,
) -> types::FinalRecoveryVerdict {
    let error_text = error.to_string();
    let attempt = match admit_attempt(
        job,
        ctx,
        step_index,
        error,
        &error_text,
        failure_code,
        budget,
    ) {
        Ok(attempt) => attempt,
        Err(verdict) => return verdict,
    };
    let recovery_input = match deterministic_recovery(&attempt) {
        Ok(input) => input,
        Err(verdict) => return verdict,
    };
    let snapshot = CheckoutSnapshot::observe(ctx, &attempt.worktree.workspace_path);
    let answer = provider_answer(&attempt, recovery_input);
    let outcome = admit_provider_answer(&attempt, &snapshot, answer);
    conclude_outcome(&attempt, outcome)
}

fn skip(
    ctx: &ExecCtx<'_>,
    step: &JobV2Step,
    activity: &ResolvedRecoveryActivity,
    reason: &str,
) -> types::FinalRecoveryVerdict {
    settlement::emit_final_recovery_event(ctx, step, activity, "skipped", None, Some(reason));
    types::FinalRecoveryVerdict::Skipped
}

/// Phase one: read the failure, run context, worktree and budget; evaluate
/// the skip guards; spend or replay the budget; verify an absorbed candidate;
/// ask the host to admit. Returns the terminal verdict or the admitted
/// attempt.
fn admit_attempt<'a, 'c>(
    job: &'a JobV2,
    ctx: &'a ExecCtx<'c>,
    step_index: usize,
    error: &'a DispatchError,
    error_message: &'a str,
    failure_code: Option<&orbit_types::workflow::StepFailureCode>,
    budget: &mut types::FinalRecoveryBudget,
) -> Result<Attempt<'a, 'c>, types::FinalRecoveryVerdict> {
    let Some(activity) = ctx.final_recovery_activity.as_ref() else {
        return Err(types::FinalRecoveryVerdict::Skipped);
    };
    let step = &job.steps[step_index];
    let budget_spent = matches!(budget, types::FinalRecoveryBudget::Spent);
    if let Some(reason) = skip_reason(budget_spent, step, error, error_message, failure_code) {
        return Err(skip(ctx, step, activity, reason));
    }
    let Some(task_id) = input::single_task_id(&ctx.input) else {
        return Err(skip(
            ctx,
            step,
            activity,
            "final recovery decides for exactly one task; this run carries none or several",
        ));
    };
    let Some(worktree) = input::run_worktree(job, ctx, step_index) else {
        return Err(skip(
            ctx,
            step,
            activity,
            "the run has no assigned worktree for final recovery to inspect",
        ));
    };
    let mut attempt = Attempt {
        job,
        ctx,
        step_index,
        step,
        activity,
        error,
        error_message,
        task_id,
        worktree,
        absorbed_refusal: None,
    };

    if let types::FinalRecoveryBudget::Replay(decision, fault) =
        std::mem::replace(budget, types::FinalRecoveryBudget::Spent)
    {
        return Err(replay_decision(&attempt, decision, fault));
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
            &attempt.task_id,
            std::path::Path::new(&attempt.worktree.workspace_path),
        )
    });
    let request = FinalRecoveryAdmissionRequest {
        task_id: attempt.task_id.clone(),
        failed_step_id: step.id.clone(),
        base_ref: attempt.worktree.base_ref.clone(),
    };
    match ctx.host.admit_final_recovery(&ctx.run_id, &request) {
        Ok(FinalRecoveryAdmission::Admitted) => {}
        Ok(FinalRecoveryAdmission::Skipped { reason }) => {
            return Err(skip(ctx, step, activity, &reason));
        }
        Err(error) => {
            return Err(skip(
                ctx,
                step,
                activity,
                &format!("admission failed: {error}"),
            ));
        }
    }
    match absorbed {
        Some(Ok(candidate)) => Err(settlement::settle_absorbed(
            job,
            ctx,
            step,
            activity,
            attempt.task_id,
            attempt.worktree,
            &candidate,
        )),
        Some(Err(refusal)) => {
            tracing::warn!(
                target: "orbit.engine.job_executor",
                run_id = %ctx.run_id,
                failed_step_id = %step.id,
                reason = refusal.reason.code(),
                detail = %refusal.detail,
                "absorbed candidate not settled without an agent"
            );
            attempt.absorbed_refusal = Some(refusal);
            Ok(attempt)
        }
        None => Ok(attempt),
    }
}

/// Why the failure never reaches final recovery, in guard order.
fn skip_reason(
    budget_spent: bool,
    step: &JobV2Step,
    error: &DispatchError,
    error_message: &str,
    failure_code: Option<&orbit_types::workflow::StepFailureCode>,
) -> Option<&'static str> {
    if budget_spent {
        return Some("final recovery already ran for this run");
    }
    // A normal base race is owned by the deterministic preparation boundary.
    // Its refusal/exhaustion preserves the candidate; an agent cannot renew
    // its host authority or replenish its bounded attempts and deadline.
    if step.id == SYNC_BASE_STEP_ID && error_message.contains("prepared_base_refresh_refused:") {
        return Some(
            "host-owned preparation recovery refused or exhausted; preserve the candidate without source repair",
        );
    }
    // No class in the one unrepairable table is judged by another agent:
    // no decision lets a lane run a required check its sandbox refuses,
    // fixes a host that lacks a tool, fixes a required command the base
    // fails the same way, clears a blocker already declared, judges a
    // candidate whose agent an upgrade refused, gives the provider's
    // model capacity, outlasts its account's usage limit, makes an
    // unusable provider usable or gets past its content policy, reaches
    // a claimed task's owner, or records the owner approval the
    // implementer is waiting on. The failure handoff keeps the candidate
    // (and a claimed leaf releases its claim). Branch on the typed code
    // the step failure carried (or the dispatch error's own); only a
    // failure with no code reaches the legacy marker fallback.
    let typed_code = failure_code.cloned().or_else(|| error.step_failure_code());
    if let Some(unrepairable) = unrepairable_failure(typed_code.as_ref(), Some(error_message)) {
        return Some(match unrepairable {
            UnrepairableFailure::ValidationRouteUnavailable => {
                "required validation cannot run in the lane that tried it and was not handed to its \
                 host replay; the candidate was not judged"
            }
            UnrepairableFailure::ValidationEnvironment => {
                "required validation lacked a tool in its environment; the candidate was not judged"
            }
            UnrepairableFailure::BaselineRed => {
                "required validation fails on the base exactly as on the candidate; the candidate \
                 did not cause it"
            }
            UnrepairableFailure::BlockedByAgent => {
                "an agent declared a blocker; the candidate was not judged by another agent"
            }
            UnrepairableFailure::UpgradePending => {
                "an Orbit upgrade refused the step's agent; the candidate was not judged"
            }
            UnrepairableFailure::ProviderCapacityExhausted => {
                "the provider reported the selected model at capacity; the candidate was not judged"
            }
            UnrepairableFailure::ProviderLimit => {
                "the provider reported its account's usage limit reached; the candidate was not judged"
            }
            UnrepairableFailure::ProviderUnavailable => {
                "the provider could not be used on this host; the candidate was not judged"
            }
            UnrepairableFailure::MuseModelAccessRefused => {
                "Muse refused access to the selected model"
            }
            UnrepairableFailure::ProviderRefusal => {
                "the provider's content policy refused the turn; the candidate was not judged"
            }
            UnrepairableFailure::OwnerRouteUnavailable => {
                "the claimed worker could not reach its owner through the run's coordinator; the \
                 candidate was not judged"
            }
            UnrepairableFailure::OwnerSignoffRequired => {
                "recorded owner approval is still missing; another agent cannot record it"
            }
            UnrepairableFailure::FootprintUnavailable => {
                "the resumed published delivery's deferred footprint stayed held past its \
                 wait; no editor started and the candidate was not judged"
            }
        });
    }
    // A `worktree_setup` that could not find a dependency is not transient:
    // the dependency is either resolvable through its registered owner or
    // it is not, and re-running unchanged cannot change that answer. No
    // recovery agent requeues it.
    if is_worktree_setup_dependency_not_found(&step.id, error) {
        return Some(
            "worktree_setup could not resolve a dependency (task not found); \
             re-running unchanged cannot resolve it",
        );
    }
    None
}

/// Hand the host the decision a run this one resumed already made.
fn replay_decision(
    attempt: &Attempt<'_, '_>,
    decision: FinalRecoveryDecision,
    fault: FaultAttribution,
) -> types::FinalRecoveryVerdict {
    // A replayed current-policy requeue of the unchanged publication
    // holds a transient completion observation as a fresh one does
    // [ORB-25661]. A replayed candidate-change escalation for the same
    // observation holds the same way as its fresh application does
    // [ORB-27875]: the read changed nothing on the forge, the recorded
    // escalation stands, and no retry is authorized. A failed
    // application keeps its failure path through
    // `held_for_completion_observation`, and any other error keeps its
    // own verdict.
    let completion_requeue = attempt.completion_observation_unavailable()
        && matches!(
            decision,
            FinalRecoveryDecision::Requeue { .. } | FinalRecoveryDecision::Escalate { .. }
        );
    let verdict = attempt.conclude(&attempt.application(decision, fault));
    if completion_requeue {
        verdict.held_for_completion_observation()
    } else {
        verdict
    }
}

/// Phase two: conclude the failures no agent should judge, or prepare the
/// provider's input. Returns the terminal verdict or the input outcome.
fn deterministic_recovery(
    attempt: &Attempt<'_, '_>,
) -> Result<Result<Value, String>, types::FinalRecoveryVerdict> {
    if let Some(verdict) = conclude_infrastructure(attempt)
        .or_else(|| conclude_rate_limit(attempt))
        .or_else(|| conclude_completion_observation(attempt))
    {
        return Err(verdict);
    }
    provider_input(attempt)
}

/// A deterministic delivery step that lost only to a lock or a locked
/// store judged nothing about the candidate, and no agent can make the
/// holder let go. The host blocks the task on that condition; once the run
/// has stopped, a passing probe requeues it onto the same candidate
/// (`orbit task recheck-blocked --confirm` and the owner's clock tick).
/// A claimed leaf is left to its agent: its task is the owner's, which
/// settles the claim through its own recovery and never sees this class.
fn conclude_infrastructure(attempt: &Attempt<'_, '_>) -> Option<types::FinalRecoveryVerdict> {
    let step = attempt.step;
    let infra_failure = match attempt.error {
        DispatchError::DeterministicActionFailed { action, message } => {
            orbit_types::workflow::TransientInfrastructure::of_action_failure(action, message)
        }
        // A tag-instructions load surfaces an owner's host-config refusal as
        // a CLI invocation failure, not a delivery action; it still judged
        // nothing about the candidate.
        DispatchError::CliInvocationFailed(message) => {
            orbit_types::workflow::TransientInfrastructure::of_action_failure(&step.id, message)
                .filter(|class| {
                    *class == orbit_types::workflow::TransientInfrastructure::HostConfiguration
                })
        }
        _ => None,
    };
    let class = infra_failure.filter(|_| !attempt.claimed())?;
    let decision = FinalRecoveryDecision::Escalate {
        diagnosis: format!(
            "step `{}` failed on transient infrastructure ({}): {}; the candidate was not \
             judged. Original error: {}",
            step.id,
            class.as_str(),
            class.describe(),
            bounded_recovery_text(
                "error",
                &attempt.ctx.run_id,
                attempt.error_message,
                4 * 1024
            ),
        ),
        human_action: "Nothing, unless it recurs: once this run has stopped and the \
                       condition clears, `orbit task recheck-blocked --confirm` (and the \
                       clock tick) requeue the task onto the candidate its failure handoff \
                       published. After the run stops, `orbit task requeue` does it at \
                       once."
            .into(),
        summary: None,
    };
    let fault = FaultAttribution::new(
        RecoveryFault::Infrastructure,
        format!(
            "run {} step `{}` failed on transient infrastructure ({})",
            attempt.ctx.run_id,
            step.id,
            class.as_str()
        ),
    );
    let application = FinalRecoveryApplication {
        infrastructure: Some(class),
        ..attempt.application(decision, fault)
    };
    Some(attempt.conclude(&application))
}

/// [ORB-25864] GitHub rate-limited the automation account's read. The
/// quota is shared by every pipeline on the host and resets within the
/// hour; no agent can outlast it, and the candidate was not judged. The
/// unchanged publication is requeued, not escalated: the next run's
/// completion waits for the reset within its own fresh budget. A claimed
/// leaf's task is its owner's, as above.
fn conclude_rate_limit(attempt: &Attempt<'_, '_>) -> Option<types::FinalRecoveryVerdict> {
    let DispatchError::DeterministicActionFailed { action, message } = attempt.error else {
        return None;
    };
    if !orbit_types::workflow::is_github_rate_limit_failure(action, message) || attempt.claimed() {
        return None;
    }
    let step = attempt.step;
    let decision = FinalRecoveryDecision::Requeue {
        reason: format!(
            "provider-rate-limit: GitHub rate-limited a read of step `{}` before the \
             automation account's quota reset; requeue the unchanged publication, whose \
             next completion waits for the reset within its own budget",
            step.id
        ),
    };
    let fault = FaultAttribution::new(
        RecoveryFault::Infrastructure,
        format!(
            "run {} step `{}` was rate-limited by GitHub: {}",
            attempt.ctx.run_id,
            step.id,
            bounded_recovery_text(
                "error",
                &attempt.ctx.run_id,
                attempt.error_message,
                MAX_RECOVERY_ERROR_MESSAGE_BYTES,
            )
        ),
    );
    Some(attempt.conclude(&attempt.application(decision, fault)))
}

/// Read-only completion transport failures spend the same durable final
/// recovery budget, but need neither an agent nor a replacement candidate.
/// The single retry waits out a short outage first; the cumulative
/// completion deadline keeps counting through the wait.
fn conclude_completion_observation(
    attempt: &Attempt<'_, '_>,
) -> Option<types::FinalRecoveryVerdict> {
    if !attempt.completion_observation_unavailable() {
        return None;
    }
    let ctx = attempt.ctx;
    let step = attempt.step;
    let error_message = attempt.error_message;
    let authentication =
        ctx.host
            .completion_observation_admission(&ctx.run_id, &attempt.task_id, &step.id);
    let refusal = match authentication {
        Ok(CompletionObservationAdmission::Retry) => {
            crate::executor::automation::vcs::fixed_wait(COMPLETION_OBSERVATION_RETRY_BACKOFF);
            let rationale =
                "transient-observation: authenticated unchanged publication; retry current completion observation"
                    .to_string();
            let application = FinalRecoveryApplication {
                resume_step_index: Some(attempt.step_index as u32),
                resume_context: Some(FinalRecoveryResume {
                    step_id: step.id.clone(),
                    failed_step_id: step.id.clone(),
                    rationale: rationale.clone(),
                    refusal: bounded_recovery_text(
                        "refusal",
                        &ctx.run_id,
                        error_message,
                        MAX_RECOVERY_ERROR_MESSAGE_BYTES,
                    ),
                    refused_output: Value::Null,
                }),
                ..attempt.application(
                    FinalRecoveryDecision::Resume {
                        step_id: step.id.clone(),
                        rationale,
                    },
                    FaultAttribution::new(
                        RecoveryFault::Infrastructure,
                        format!(
                            "run {} step `{}` could not observe completion of an unchanged, \
                             authenticated publication",
                            ctx.run_id, step.id
                        ),
                    ),
                )
            };
            return Some(settlement::conclude(
                attempt.job,
                ctx,
                step,
                attempt.activity,
                &application,
                Some(attempt.step_index),
                None,
            ));
        }
        Ok(CompletionObservationAdmission::PolicyChanged { reason }) => {
            // The ordinary bounded requeue preserves the publication and
            // admits a fresh run under current policy; no old gate is reused.
            let application = attempt.application(
                FinalRecoveryDecision::Requeue {
                    reason: format!(
                        "policy-changed: {reason}; transient completion observation requires current-policy source repair"
                    ),
                },
                FaultAttribution::unknown(format!(
                    "run {} step `{}` could not observe completion and its policy changed \
                     since publication",
                    ctx.run_id, step.id
                )),
            );
            return Some(
                attempt
                    .conclude(&application)
                    .held_for_completion_observation(),
            );
        }
        Ok(CompletionObservationAdmission::Stopped { reason }) => {
            // The worker is gone, so the in-worker retry cannot run. Requeue
            // the unchanged publication under current policy. This is not a
            // candidate change and it does not ask for a manual inspect.
            let application = attempt.application(
                FinalRecoveryDecision::Requeue {
                    reason: format!(
                        "stopped-run: {reason}; requeue the unchanged publication under current policy"
                    ),
                },
                FaultAttribution::unknown(format!(
                    "run {} step `{}` could not observe completion of an unchanged publication \
                     after the run stopped: {}",
                    ctx.run_id,
                    step.id,
                    bounded_recovery_text(
                        "error",
                        &ctx.run_id,
                        error_message,
                        MAX_RECOVERY_ERROR_MESSAGE_BYTES,
                    )
                )),
            );
            return Some(
                attempt
                    .conclude(&application)
                    .held_for_completion_observation(),
            );
        }
        Ok(CompletionObservationAdmission::Unavailable) => {
            "host has no authenticated completion prefix".to_string()
        }
        Err(error) => error.to_string(),
    };
    // A refusal must not reach an agent that can request the same stale
    // prefix again. Preserve the publication for explicit governed repair.
    // [ORB-27875] The escalation stays recorded and nothing is retried,
    // but a read-only observation that changed nothing on the forge ends
    // the run held, not failed, when the host applied the park.
    let application = attempt.application(
        FinalRecoveryDecision::Escalate {
            diagnosis: format!("unchanged completion reuse refused: candidate-changed: {refusal}; original error: {error_message}"),
            human_action: "Inspect the publication and admit current-policy source repair when its evidence or inputs changed.".into(), summary: None,
        },
        FaultAttribution::unknown(format!(
            "run {} step `{}` could not observe completion and reuse of its publication \
             was refused",
            ctx.run_id, step.id
        )),
    );
    Some(
        attempt
            .conclude(&application)
            .held_for_completion_observation(),
    )
}

/// The provider's input for a failure no deterministic case concluded, or the
/// reason it could not be built. A completion that spent its watch budget
/// concludes here instead.
fn provider_input(
    attempt: &Attempt<'_, '_>,
) -> Result<Result<Value, String>, types::FinalRecoveryVerdict> {
    let ctx = attempt.ctx;
    let step = attempt.step;
    match input::final_recovery_input(
        attempt.job,
        ctx,
        attempt.step_index,
        &attempt.task_id,
        &attempt.worktree,
        attempt.error_message,
    ) {
        Ok(input::FinalRecoveryInput::Wait(reason)) => {
            // Completion spent its watch budget. Preserve the publication and
            // use the existing bounded requeue, rather than starting another
            // watch or asking an agent to diagnose unconcluded checks.
            let application = attempt.application(
                FinalRecoveryDecision::Requeue { reason },
                FaultAttribution::unknown(format!(
                    "run {} step `{}` spent its watch budget before the required checks \
                     concluded",
                    ctx.run_id, step.id
                )),
            );
            Err(attempt.conclude(&application))
        }
        Ok(input::FinalRecoveryInput::Ready(mut input)) => {
            if let Some(refusal) = &attempt.absorbed_refusal {
                input["absorbed_refusal"] = refusal.evidence();
            }
            Ok(Ok(input))
        }
        Err(error) => Ok(Err(error)),
    }
}

/// Phase three, dispatch: ask the provider and read its answer. A dispatch
/// that failed or a decision its parser refused escalates.
fn provider_answer(attempt: &Attempt<'_, '_>, input: Result<Value, String>) -> ProviderAnswer {
    let step = attempt.step;
    let error_message = attempt.error_message;
    let dispatch = input.and_then(|input| {
        decision::dispatch_final_recovery(attempt.ctx, step, attempt.activity, &input)
    });
    match dispatch {
        Ok(dispatched) => {
            let malformed = FinalRecoveryDecision::parse_attributed(&dispatched.payload).is_err();
            let (mut decision, fault) =
                FinalRecoveryDecision::from_attributed_output(Some(&dispatched.payload));
            // A malformed answer escalates, and nothing in it names what
            // failed. Carry the original failed step and its bounded,
            // redacted error next to the parser error, so the operator sees
            // both failures in one handoff. The decision stays an
            // escalation: the refused response authorizes nothing.
            if malformed && let FinalRecoveryDecision::Escalate { diagnosis, .. } = &mut decision {
                append_original_failure(diagnosis, step, error_message);
            }
            ProviderAnswer {
                decision,
                fault: fault.judged_by(dispatched.judge),
                model: dispatched.model,
                repair_evidence: dispatched.repair_evidence,
            }
        }
        // No usable answer reaches here: a dispatch that failed or a decision
        // refused by its parser. Like a malformed answer above, it names the
        // failed step, so the one escalation carries both failures.
        Err(message) => {
            let mut diagnosis = format!(
                "final recovery activity `{}` did not return a decision: {message}",
                attempt.activity.name
            );
            append_original_failure(&mut diagnosis, step, error_message);
            ProviderAnswer {
                decision: FinalRecoveryDecision::Escalate {
                    diagnosis,
                    human_action:
                        "Inspect the failed run and its final-recovery invocation, then move \
                                   the task by hand."
                            .to_string(),
                    summary: None,
                },
                fault: FaultAttribution::unknown(format!(
                    "final recovery activity `{}` for run {} returned no decision",
                    attempt.activity.name, attempt.ctx.run_id
                )),
                model: None,
                repair_evidence: None,
            }
        }
    }
}

/// Phase three, admission: admit the resume step, commit the repair, observe
/// it and write the delayed evidence. Any refusal escalates and drops the
/// resume.
fn admit_provider_answer(
    attempt: &Attempt<'_, '_>,
    snapshot: &CheckoutSnapshot,
    answer: ProviderAnswer,
) -> RecoveryOutcome {
    let job = attempt.job;
    let ProviderAnswer {
        decision,
        fault,
        model,
        repair_evidence,
    } = answer;
    let (mut decision, mut resume_index, downgrade) =
        decision::admit_resume_step(job, attempt.step_index, decision);
    let commit_index = job.steps[1..=attempt.step_index]
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
            attempt.ctx,
            recovery::FinalRecoveryRepair {
                step: attempt.step,
                activity: attempt.activity,
                commit_step: &job.steps[index],
                task_id: &attempt.task_id,
                worktree: &attempt.worktree,
                source_before: snapshot.ownership(),
                recovery_model: model.as_deref(),
            },
        )
    {
        decision = refused_repair_escalation(attempt, &refusal);
        resume_index = None;
    }
    let repair_commit = resume_index.and_then(|_| {
        settlement::observe_repair_commit(
            &attempt.ctx.host.process_env(),
            &attempt.worktree.workspace_path,
            snapshot.head.as_deref(),
        )
    });
    if repair_commit.is_some()
        && let Some(commit_index) = commit_index
    {
        // A repair is a new candidate. Even a request to resume settlement
        // must rerun commit, validation, admission and review first.
        let index = resume_index.unwrap_or(attempt.step_index).min(commit_index);
        if let FinalRecoveryDecision::Resume { step_id, .. } = &mut decision {
            *step_id = job.steps[index].id.clone();
        }
        resume_index = Some(index);
    }
    // Evidence is written only once its resume is admitted: the step and any
    // repair commit have passed their gates. A refused write escalates with the
    // failed step's diagnostic. The repair commit that was observed stays on the
    // record, since the candidate's HEAD has moved.
    if let Some(evidence) = repair_evidence
        && resume_index.is_some()
        && let Err(error) = decision::persist_repair_evidence(
            attempt.ctx,
            &attempt.task_id,
            &attempt.worktree.workspace_path,
            &evidence,
        )
    {
        let mut diagnosis = error;
        append_original_failure(&mut diagnosis, attempt.step, attempt.error_message);
        decision = FinalRecoveryDecision::Escalate {
            diagnosis,
            human_action: "Inspect the refused repair evidence and its candidate before resuming \
                           the run."
                .into(),
            summary: None,
        };
        resume_index = None;
    }
    RecoveryOutcome {
        decision,
        fault,
        resume_index,
        downgrade,
        repair_commit,
    }
}

/// The escalation for a repair its commit step refused, with the uncommitted
/// diff saved for the operator.
fn refused_repair_escalation(
    attempt: &Attempt<'_, '_>,
    refusal: &impl std::fmt::Display,
) -> FinalRecoveryDecision {
    let ctx = attempt.ctx;
    let artifact_name = if attempt.step.id == "landing_review" {
        format!("landing-recovery-uncommitted-{}.patch", ctx.run_id)
    } else {
        format!("recovery-uncommitted-{}.patch", ctx.run_id)
    };
    let saved_diff = match crate::executor::automation::vcs::persist_uncommitted_diff_artifact(
        ctx.host,
        &attempt.task_id,
        std::path::Path::new(&attempt.worktree.workspace_path),
        &artifact_name,
    ) {
        Ok(Some(location)) => format!("; uncommitted diff saved to {location}"),
        Ok(None) => String::new(),
        Err(error) => format!("; failed to capture uncommitted diff: {error}"),
    };
    FinalRecoveryDecision::Escalate {
        diagnosis: format!(
            "{refusal}{saved_diff}; original error before recovery: {}",
            attempt.error_message
        ),
        human_action:
            "Inspect the refused repair and its candidate ownership before resuming the run.".into(),
        summary: None,
    }
}

/// Phase three, application: hand the admitted outcome to the host.
fn conclude_outcome(
    attempt: &Attempt<'_, '_>,
    outcome: RecoveryOutcome,
) -> types::FinalRecoveryVerdict {
    let RecoveryOutcome {
        decision,
        fault,
        resume_index,
        downgrade,
        repair_commit,
    } = outcome;
    let ctx = attempt.ctx;
    let step = attempt.step;
    // The context names the admitted step, not the decision's text, which
    // admission matched after trimming. The resumed agent gets the same bounded
    // copies final recovery read: an unbounded diagnostic would make the
    // provider refuse the resumed turn before it starts [ORB-12467]. The full
    // error stays in the step record.
    let resume_context = match (&decision, resume_index) {
        (FinalRecoveryDecision::Resume { rationale, .. }, Some(index)) => {
            Some(FinalRecoveryResume {
                step_id: attempt.job.steps[index].id.clone(),
                failed_step_id: step.id.clone(),
                rationale: rationale.clone(),
                refusal: bounded_recovery_text(
                    "refusal",
                    &ctx.run_id,
                    attempt.error_message,
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
        decision::preserved_publication_steps(attempt.job, ctx, &attempt.task_id, &attempt.worktree)
    } else {
        Vec::new()
    };
    let requeued = matches!(decision, FinalRecoveryDecision::Requeue { .. });
    let application = FinalRecoveryApplication {
        resume_step_index: resume_index.map(|index| index as u32),
        preserved_steps,
        repair_commit,
        resume_context,
        ..attempt.application(decision, fault)
    };
    let verdict = settlement::conclude(
        attempt.job,
        ctx,
        step,
        attempt.activity,
        &application,
        resume_index,
        downgrade.as_deref(),
    );
    // A recovery agent cannot be dispatched for a completion observation
    // (the deterministic phase intercepts it), but a requeue decided on this
    // path for one holds the same way.
    if attempt.completion_observation_unavailable() && requeued {
        verdict.held_for_completion_observation()
    } else {
        verdict
    }
}

/// Name the failed step and its bounded, redacted original failure in an
/// escalation that could not use the agent's answer [ORB-27944]. The host
/// receives one escalation carrying the refusal and the failure it answered.
fn append_original_failure(diagnosis: &mut String, step: &JobV2Step, error_message: &str) {
    diagnosis.push_str(&format!(
        "; the original failure was step `{}`: {}",
        step.id,
        redacted_recovery_diagnostic(error_message)
    ));
}

/// Whether `error` is a `worktree_setup` failure caused by a dependency
/// lookup that found no task. Only the step's own deterministic failure
/// counts, and only when it carries the remote `not_found` / `task not
/// found` text the owner lookup produces.
fn is_worktree_setup_dependency_not_found(step_id: &str, error: &DispatchError) -> bool {
    if step_id != WORKTREE_SETUP_STEP_ID {
        return false;
    }
    let message = error.to_string();
    message.contains("task not found")
        && (message.contains("not_found") || message.contains("NotFound"))
}
