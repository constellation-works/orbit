//! The settlement a terminal leaf implies, and the release evidence the owner
//! carries back to its task.
use crate::OrbitRuntime;
use std::path::Path;

use orbit_common::text::floor_char_boundary;
use orbit_store::contracts::{
    ClaimCandidateRef, ClaimEvidence, ClaimFailure, ClaimFinalRecovery, ClaimMutation,
    ClaimPullRequest, LocalPullAdmission, LocalPullPhase, ProviderUnavailable,
};
use orbit_types::workflow::{
    BASELINE_RED_MARKER, BaselineRedHold, ClaimFailureClass, FORGE_UNAVAILABLE_MARKER,
    FinalRecoveryDecision, ForgeUnavailableHold, JobRunState, OWNER_ROUTE_UNAVAILABLE_MARKER,
    PROVIDER_CAPACITY_MARKER, PROVIDER_LIMIT_MARKER, PROVIDER_UNAVAILABLE_MARKER, PipelineState,
    ReviewEvidenceHold, TRANSIENT_FAILURE_MARKER, UPGRADE_PENDING_MARKER,
    VALIDATION_ENVIRONMENT_MARKER, is_baseline_red_failure, is_forge_unavailable,
    is_provider_limit, is_provider_refusal,
};

use super::super::candidate::{
    SYNC_BASE_STEP, candidate_note, first_incomplete_step, preserved_candidate,
};

/// Largest failure excerpt a settlement carries. The whole diagnostic stays in
/// the executor's run record; the owner's reader needs enough to decide.
const MAX_FAILURE_EXCERPT_BYTES: usize = 8 * 1024;

/// The settlement a terminal leaf implies, by how far its admission got: a
/// leaf that never launched was cancelled while queued, so nothing ran and
/// its claim is released back to the owner's backlog. A launched leaf ended
/// without the typed handoff success records, and its settlement carries why
/// as a typed [`ClaimFailure`] [ORB-14257]: a class that
/// [blocks](ClaimFailureClass::blocks) — the candidate's, or the task's own —
/// fails the claim, as does an operator's cancel that asked to block the task
/// [ORB-14274]; any other (an operator's cancel, a provider this host
/// could not use [ORB-13941] or whose content policy refused the task
/// [ORB-15573], a missing validation tool, an unreachable owner, a red base,
/// an inconclusive or interrupted run) releases it, and the drain excludes
/// the leaf's crew for its window when the failure
/// [says so](ClaimFailure::excludes_crew).
///
/// Every follower process that settles a terminal leaf computes it here, so
/// the leaf's own worker, a cancel and a drain pass agree on the value.
/// `diagnostic` is the `(code, message)` the terminalizing caller knows before
/// its diagnostic step is durable; `state` is the leaf's pipeline state.
///
/// [ORB-15956] A red base holds the task only while it is still red:
/// `base_no_longer_red` says why a leaf's recorded red base is not, and the
/// release then keeps the candidate without the hold, so the task's next
/// claim resumes it on the current base instead of waiting.
///
/// [ORB-13907] The leaf's recorded final recovery decision rides on a failure
/// settlement for the owner to apply — a follower never writes its owner's
/// task — unless it was `resume`, whose rerun then failed on its own.
///
/// A `held` leaf whose review settled into an evidence hold did not fail; it
/// is released with the typed hold ([`evidence_hold_release`]). One held
/// because the forge kept refusing its push past the push's retry window is
/// a `transient` release that carries the forge hold
/// ([`forge_hold_release`]), which excludes neither the crew nor the host.
pub(crate) fn leaf_failure_settlement(
    record: &LocalPullAdmission,
    run: &orbit_types::workflow::JobRun,
    diagnostic: Option<(&str, &str)>,
    state: Option<&PipelineState>,
    base_no_longer_red: &dyn Fn(&BaselineRedHold) -> Option<String>,
) -> ClaimMutation {
    if matches!(
        record.phase,
        LocalPullPhase::Created | LocalPullPhase::Bound
    ) {
        return release_settlement(
            record,
            &format!(
                "its queued leaf {} terminated as {} before launch",
                run.run_id, run.state
            ),
        );
    }
    if run.state == JobRunState::Held
        && let Some(hold) = held_evidence(state)
    {
        return evidence_hold_release(record, run, hold, held_pull_request(state));
    }
    let final_recovery = state
        .and_then(|state| state.final_recovery.as_ref())
        .and_then(|checkpoint| checkpoint.decision.clone())
        .filter(|decision| !matches!(decision, FinalRecoveryDecision::Resume { .. }));
    let failure = leaf_failure(record, run, diagnostic, final_recovery.as_ref(), state);
    // [ORB-14274] An operator who cancelled with `--block` asked for the
    // legacy outcome: the cancel stays typed but fails the claim.
    let operator_blocks = run.state == JobRunState::Cancelled
        && state
            .and_then(|state| state.task_cancellation_policy.as_ref())
            .is_some_and(|policy| policy.block);
    if !failure.class.blocks() && !operator_blocks {
        // [ORB-14258] A required command the base fails exactly as the
        // candidate does releases with the typed hold, which the owner
        // records so its admission waits for the base to move.
        let mut hold = (failure.class == ClaimFailureClass::BaselineRed)
            .then(|| baseline_red(run, diagnostic))
            .flatten();
        // [ORB-15956] Only a base that is still red holds the task.
        let green = hold.as_ref().and_then(|red| {
            base_no_longer_red(red).map(|why| base_green_release(record, run, red, &why, &failure))
        });
        if green.is_some() {
            hold = None;
        }
        // [ORB-14634] A leaf the forge kept refusing held its claim for its
        // push's retry window; the release says so and names the head.
        let forge = (failure.class == ClaimFailureClass::Transient
            && run.state == JobRunState::Held)
            .then(|| forge_hold(run, diagnostic, state))
            .flatten();
        let mut evidence = match (&hold, &forge, green) {
            (Some(hold), _, _) => baseline_red_release(record, run, hold, &failure),
            (None, _, Some(green)) => green,
            (None, Some(forge), None) => forge_hold_release(record, run, forge, &failure),
            (None, None, None) => release_evidence(record, &release_reason(run, &failure)),
        };
        evidence.baseline_red = hold;
        evidence.forge_hold = forge;
        // [ORB-15573] A refusal is no unavailability: the owner holds the
        // task back from the provider instead.
        if failure.class == ClaimFailureClass::Provider && !failure.provider_refusal {
            evidence.provider_unavailable = Some(ProviderUnavailable {
                crew: failure.crew.clone(),
                reason: failure.reason.clone(),
            });
        }
        evidence.failure = Some(failure);
        return ClaimMutation::Release(evidence);
    }
    let mut summary = terminal_failure_summary_with(run, diagnostic);
    let final_recovery = final_recovery.map(|decision| {
        summary.push_str(&format!(
            "\nFinal recovery decided `{}`; the owner applies it.",
            decision.kind()
        ));
        ClaimFinalRecovery {
            run_id: run.run_id.clone(),
            decision,
        }
    });
    ClaimMutation::Fail(ClaimEvidence {
        summary: Some(summary),
        final_recovery,
        failure: Some(failure),
        ..Default::default()
    })
}

/// The evidence hold a held leaf's review settlement recorded in its run
/// state, under its before-PR or before-landing settlement step's output.
fn held_evidence(state: Option<&PipelineState>) -> Option<ReviewEvidenceHold> {
    let pipeline = &state?.pipeline;
    ["landing_review_gate_settle", "review_gate_settle"]
        .into_iter()
        .find_map(|step| {
            let hold = pipeline.get(step)?.get("evidence_hold")?;
            serde_json::from_value(hold.clone()).ok()
        })
}

/// [ORB-15570] The pull request a leaf held in its before-landing review
/// opened at `pr_open`, which stays open under the hold.
fn held_pull_request(state: Option<&PipelineState>) -> Option<ClaimPullRequest> {
    let opened = state?.pipeline.get("pr_open")?;
    let text = |field: &str| {
        opened
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Some(ClaimPullRequest {
        number: text("pr_number")?,
        head: text("head")?,
    })
}

/// A `held` leaf whose review settled into an evidence hold did not fail: its
/// claim is released with the typed hold, which the owner records as the
/// task's latest decision, and the pull request a before-landing hold left
/// open, which the owner keeps with the held candidate.
fn evidence_hold_release(
    record: &LocalPullAdmission,
    run: &orbit_types::workflow::JobRun,
    hold: ReviewEvidenceHold,
    pull_request: Option<ClaimPullRequest>,
) -> ClaimMutation {
    let drain = &record.request.run_context.run_id;
    let machine = &record.destination.execution_machine_id;
    let names = hold
        .requirements
        .iter()
        .map(|requirement| format!("`{}`", requirement.name))
        .collect::<Vec<_>>()
        .join(", ");
    let why = format!(
        "leaf {} held its reviewed candidate {} for named external evidence ({names})",
        run.run_id, hold.candidate.commit
    );
    let opened = pull_request
        .as_ref()
        .map_or_else(String::new, |pull_request| {
            format!(
                " Its pull request #{} on `{}` stays open until the run that resumes the held \
             candidate opens its own and closes it as superseded.",
                pull_request.number, pull_request.head
            )
        });
    ClaimMutation::Release(ClaimEvidence {
        summary: Some(format!("released by follower drain {drain}: {why}")),
        comment: Some(format!(
            "Follower drain {drain} on {machine} released this claim: {why}. The task stays in \
             progress under the evidence hold; attaching every named result queues a fresh \
             review. Delivery remains gated until the named evidence arrives.{opened}"
        )),
        evidence_hold: Some(hold),
        held_pull_request: pull_request,
        ..Default::default()
    })
}

/// Largest reason a typed failure carries.
const MAX_FAILURE_REASON_BYTES: usize = 1024;

/// Why a launched leaf ended, typed. An operator's cancel wins; then the
/// typed marker of the last failed step, of any provider or red-base failure
/// the run recorded, or of the terminalizing caller's diagnostic; then a
/// worker that died; then a final recovery that judged the task itself; then
/// a committed candidate that could not be synchronized onto its base.
/// Anything else is the candidate's.
fn leaf_failure(
    record: &LocalPullAdmission,
    run: &orbit_types::workflow::JobRun,
    diagnostic: Option<(&str, &str)>,
    final_recovery: Option<&FinalRecoveryDecision>,
    state: Option<&PipelineState>,
) -> ClaimFailure {
    let last_failed = run
        .steps
        .iter()
        .rev()
        .find(|step| step.error_code.is_some() || step.error_message.is_some());
    let step_class = |step: &orbit_types::workflow::JobRunStep| {
        ClaimFailureClass::of_step_failure(
            step.error_code.as_deref(),
            step.error_message.as_deref(),
        )
        .map(|class| (class, step.error_message.clone().unwrap_or_default()))
    };
    let typed = if run.state == JobRunState::Cancelled {
        // The operator's recorded cancel note names who stopped it and why.
        let reason = state
            .and_then(|state| state.task_cancellation_policy.as_ref())
            .map(|policy| policy.note.clone())
            .or_else(|| diagnostic.map(|(_, message)| message.to_string()))
            .unwrap_or_else(|| format!("leaf {} was cancelled", run.run_id));
        Some((ClaimFailureClass::OperatorCancel, reason))
    } else {
        last_failed
            .and_then(step_class)
            .or_else(|| {
                run.steps.iter().rev().find_map(|step| {
                    step_class(step).filter(|(class, _)| {
                        matches!(
                            class,
                            ClaimFailureClass::Provider | ClaimFailureClass::BaselineRed
                        )
                    })
                })
            })
            .or_else(|| {
                diagnostic.and_then(|(code, message)| {
                    ClaimFailureClass::of_step_failure(Some(code), Some(message))
                        .map(|class| (class, message.to_string()))
                })
            })
    };
    let stopped_at = state.and_then(|state| first_incomplete_step(run, state));
    // [ORB-14695] A provider's usage limit is read off its typed marker
    // before the reason drops it. [ORB-15573] A refusal keeps its marker,
    // which names the provider the owner's hold excludes.
    let provider_limit = typed.as_ref().is_some_and(|(class, reason)| {
        *class == ClaimFailureClass::Provider && is_provider_limit(None, Some(reason))
    });
    let provider_refusal = typed.as_ref().is_some_and(|(class, reason)| {
        *class == ClaimFailureClass::Provider && is_provider_refusal(None, Some(reason))
    });
    let (class, reason) = typed.unwrap_or_else(|| {
        let reason = last_failed
            .and_then(|step| step.error_message.clone())
            .or_else(|| diagnostic.map(|(_, message)| message.to_string()))
            .unwrap_or_else(|| format!("leaf {} terminated as {}", run.run_id, run.state));
        let class = if run.state == JobRunState::Interrupted {
            ClaimFailureClass::Transient
        } else if matches!(
            final_recovery,
            Some(FinalRecoveryDecision::Reject { .. } | FinalRecoveryDecision::Archive { .. })
        ) {
            ClaimFailureClass::TaskInput
        } else if stopped_at == Some(SYNC_BASE_STEP) {
            // The committed candidate is intact; only the base moved under
            // it, and the step's conflict recovery could not carry it over.
            ClaimFailureClass::BaseConflict
        } else {
            ClaimFailureClass::Candidate
        };
        (class, reason)
    });
    // [ORB-14617] A forge hold's text leads with its JSON; say it plainly.
    let mut reason = match ForgeUnavailableHold::from_text(&reason) {
        Some(hold) => format!(
            "the forge refused the push of {} to {} {} times over {} s",
            hold.head_sha,
            hold.target_ref,
            hold.attempts,
            hold.waited_ms / 1000
        ),
        None => reason,
    };
    for marker in FAILURE_MARKERS {
        reason = reason.replace(marker, "");
    }
    if let Some(carry) = state
        .and_then(|state| state.failure_activity_checkpoint.as_ref())
        .filter(|checkpoint| checkpoint.activity_name == "claim_candidate_carry")
        .map(|checkpoint| &checkpoint.output)
        && carry.get("carry").and_then(serde_json::Value::as_str) == Some("failed")
        && let Some(refusal) = carry.get("reason").and_then(serde_json::Value::as_str)
    {
        reason = format!("candidate carry failed: {refusal}; original failure: {reason}");
    }
    let reason = reason.trim();
    let cut = floor_char_boundary(reason, MAX_FAILURE_REASON_BYTES);
    let crew = run
        .resolved_crew
        .clone()
        .filter(|crew| !crew.trim().is_empty())
        .or_else(|| {
            record
                .receipt
                .as_ref()
                .and_then(|receipt| receipt.task.as_ref())
                .and_then(|task| task.crew.clone())
        });
    ClaimFailure {
        class,
        reason: reason[..cut].to_string(),
        crew,
        candidate: state.and_then(|state| preserved_candidate(run, state)),
        provider_limit,
        provider_refusal,
    }
}

/// Orbit's typed failure markers, which a failure's reason quotes without.
const FAILURE_MARKERS: [&str; 9] = [
    FORGE_UNAVAILABLE_MARKER,
    PROVIDER_UNAVAILABLE_MARKER,
    PROVIDER_CAPACITY_MARKER,
    PROVIDER_LIMIT_MARKER,
    VALIDATION_ENVIRONMENT_MARKER,
    OWNER_ROUTE_UNAVAILABLE_MARKER,
    BASELINE_RED_MARKER,
    TRANSIENT_FAILURE_MARKER,
    UPGRADE_PENDING_MARKER,
];

/// The baseline hold a terminal leaf failed on: a failed step, or the
/// terminalizing caller's own diagnostic, carrying the typed
/// `[baseline_red]` failure `claim_validate` raises.
fn baseline_red(
    run: &orbit_types::workflow::JobRun,
    diagnostic: Option<(&str, &str)>,
) -> Option<BaselineRedHold> {
    run.steps
        .iter()
        .rev()
        .find(|step| {
            is_baseline_red_failure(step.error_code.as_deref(), step.error_message.as_deref())
        })
        .and_then(|step| step.error_message.as_deref())
        .and_then(BaselineRedHold::from_text)
        .or_else(|| {
            diagnostic
                .filter(|(code, message)| is_baseline_red_failure(Some(code), Some(message)))
                .and_then(|(_, message)| BaselineRedHold::from_text(message))
        })
        .map(|mut hold| {
            if hold.run_id.is_empty() {
                hold.run_id.clone_from(&run.run_id);
            }
            hold
        })
}

/// [ORB-14258] The release of a leaf whose required command fails on its
/// base exactly as on its candidate: the owner holds the task until the base
/// moves to a commit where the command passes.
fn baseline_red_release(
    record: &LocalPullAdmission,
    run: &orbit_types::workflow::JobRun,
    hold: &BaselineRedHold,
    failure: &ClaimFailure,
) -> ClaimEvidence {
    let drain = &record.request.run_context.run_id;
    let machine = &record.destination.execution_machine_id;
    let why = format!(
        "leaf {} ended on a `{}` failure: required validation `{}` is red on base {} exactly \
         as on its candidate{}",
        run.run_id,
        failure.class.as_str(),
        hold.command,
        hold.base_sha,
        candidate_note(failure)
    );
    let base_ref = if hold.base_ref.is_empty() {
        "the base".to_string()
    } else {
        format!("`{}`", hold.base_ref)
    };
    ClaimEvidence {
        summary: Some(format!("released by follower drain {drain}: {why}")),
        comment: Some(format!(
            "Follower drain {drain} on {machine} released this claim: {why}. The task is back \
             in the backlog, held until {base_ref} moves to a base where the command passes; no \
             pull request was opened."
        )),
        ..Default::default()
    }
}

/// [ORB-15956] Why `hold`'s base is no longer red in `repo`, from a result
/// already recorded for its base ref's current tip. Never runs the command:
/// a settlement must not wait on a build, and a moved tip nobody checked
/// keeps the hold, which the owner's hold refresh checks without a leaf.
pub(crate) fn recorded_base_no_longer_red(
    runtime: &OrbitRuntime,
    repo: &Path,
    hold: &BaselineRedHold,
) -> Option<String> {
    match orbit_engine::recorded_baseline_hold_status(runtime, repo, hold)? {
        orbit_engine::BaselineHoldStatus::Lifted(why) => Some(why),
        orbit_engine::BaselineHoldStatus::Holding(_) => None,
    }
}

/// [ORB-15956] The release of a leaf whose required command failed on a base
/// that is red no longer: the task returns to the backlog with no hold and
/// keeps its candidate, which its next claim resumes on the current base.
fn base_green_release(
    record: &LocalPullAdmission,
    run: &orbit_types::workflow::JobRun,
    hold: &BaselineRedHold,
    why: &str,
    failure: &ClaimFailure,
) -> ClaimEvidence {
    let drain = &record.request.run_context.run_id;
    let machine = &record.destination.execution_machine_id;
    let why = format!(
        "leaf {} ended on a `{}` failure: required validation `{}` was red on base {} exactly \
         as on its candidate, but that base is red no longer ({why}){}",
        run.run_id,
        failure.class.as_str(),
        hold.command,
        hold.base_sha,
        candidate_note(failure)
    );
    ClaimEvidence {
        summary: Some(format!("released by follower drain {drain}: {why}")),
        comment: Some(format!(
            "Follower drain {drain} on {machine} released this claim: {why}. The task is back \
             in the backlog with no baseline hold; its next claim resumes the kept candidate on \
             the current base."
        )),
        ..Default::default()
    }
}

/// The forge hold a held leaf ended on: the one its run state records, else
/// the one its held step, or the terminalizing caller's diagnostic, carries.
fn forge_hold(
    run: &orbit_types::workflow::JobRun,
    diagnostic: Option<(&str, &str)>,
    state: Option<&PipelineState>,
) -> Option<ForgeUnavailableHold> {
    state
        .and_then(|state| state.pull_drain.forge_hold.clone())
        .or_else(|| {
            run.steps
                .iter()
                .rev()
                .filter(|step| {
                    is_forge_unavailable(step.error_code.as_deref(), step.error_message.as_deref())
                })
                .find_map(|step| ForgeUnavailableHold::from_text(step.error_message.as_deref()?))
        })
        .or_else(|| {
            diagnostic
                .filter(|(code, message)| is_forge_unavailable(Some(code), Some(message)))
                .and_then(|(_, message)| ForgeUnavailableHold::from_text(message))
        })
}

/// [ORB-14634] The release of a leaf that held its claim while the forge
/// refused its push, once the push's retry window closed. The forge refused
/// it, not this host or the crew, so the drain keeps offering both.
fn forge_hold_release(
    record: &LocalPullAdmission,
    run: &orbit_types::workflow::JobRun,
    hold: &ForgeUnavailableHold,
    failure: &ClaimFailure,
) -> ClaimEvidence {
    let drain = &record.request.run_context.run_id;
    let machine = &record.destination.execution_machine_id;
    let crew = failure
        .crew
        .as_deref()
        .map_or_else(|| "its crew".to_string(), |crew| format!("crew `{crew}`"));
    let minutes = hold
        .held_at
        .signed_duration_since(hold.held_since)
        .num_minutes()
        .max(0);
    let why = format!(
        "leaf {} held this claim while the forge refused the push of its reviewed head {} to \
         {} ({} attempts over {minutes} min since {}), and released it when the push's retry \
         window closed{}",
        run.run_id,
        hold.head_sha,
        hold.target_ref,
        hold.attempts,
        hold.held_since.to_rfc3339(),
        candidate_note(failure)
    );
    ClaimEvidence {
        summary: Some(format!("released by follower drain {drain}: {why}")),
        comment: Some(format!(
            "Follower drain {drain} on {machine} released this claim: {why}. The forge, not this \
             host or {crew}, refused the push, so the drain keeps offering both. The task is back \
             in the backlog and can be pulled again."
        )),
        ..Default::default()
    }
}

/// The release comment's reason for a typed failure.
fn release_reason(run: &orbit_types::workflow::JobRun, failure: &ClaimFailure) -> String {
    let crew = failure.crew.as_deref().unwrap_or("its crew");
    let mut why = match failure.class {
        ClaimFailureClass::Provider if failure.provider_limit => format!(
            "leaf {} ended on a `provider` failure: the provider of crew `{crew}` reported its \
             account's usage limit on this host ({}); the work was not attempted and the \
             release does not count against the task's release budget",
            run.run_id, failure.reason
        ),
        ClaimFailureClass::Provider if failure.provider_refusal => format!(
            "leaf {} ended on a `provider` failure: the provider of crew `{crew}` refused the \
             task's content ({}); the work was not judged, and the owner holds the task back \
             from that provider's crews",
            run.run_id, failure.reason
        ),
        ClaimFailureClass::Provider => format!(
            "leaf {} ended on a `provider` failure: it could not use the provider of crew \
             `{crew}` on this host ({}); the work was not attempted",
            run.run_id, failure.reason
        ),
        ClaimFailureClass::OperatorCancel => {
            format!("its leaf {} was cancelled ({})", run.run_id, failure.reason)
        }
        ClaimFailureClass::BaseConflict => format!(
            "leaf {} ended on a `base_conflict` failure: its committed candidate could not be \
             synchronized onto a base that moved under it ({})",
            run.run_id, failure.reason
        ),
        class => format!(
            "leaf {} ended on a `{}` failure that is not the candidate's ({})",
            run.run_id,
            class.as_str(),
            failure.reason
        ),
    };
    if failure.class.suppresses_host() {
        why.push_str(", and this drain claims no more work on this host in its window");
    } else if failure.provider_limit {
        why.push_str(", and this drain runs no more tasks on that provider's crews in its window");
    } else if failure.excludes_crew() {
        why.push_str(&format!(
            ", and this drain runs no more `{crew}` tasks in its window"
        ));
    }
    why.push_str(&candidate_note(failure));
    why
}

/// Hand an unfinished claim back to the owner: the task returns to the
/// backlog, and the comment names the drain that held it and why it gave it
/// back. Nothing is recorded as a failure — the work either never ran or was
/// stopped on purpose.
pub(crate) fn release_settlement(record: &LocalPullAdmission, why: &str) -> ClaimMutation {
    ClaimMutation::Release(release_evidence(record, why))
}

/// [`release_settlement`] for a launched leaf an operator stopped: typed
/// [`ClaimFailureClass::OperatorCancel`], so the owner returns the task to
/// backlog with the cancel reason and counts the release against its budget.
/// [ORB-15314] It keeps the leaf's committed `candidate`, as the leaf's own
/// cancel settlement would, so the task's next claim continues it.
pub(crate) fn operator_cancel_release(
    record: &LocalPullAdmission,
    why: &str,
    candidate: Option<ClaimCandidateRef>,
) -> ClaimMutation {
    let failure = ClaimFailure {
        class: ClaimFailureClass::OperatorCancel,
        reason: why.to_string(),
        crew: record
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.task.as_ref())
            .and_then(|task| task.crew.clone()),
        candidate,
        provider_limit: false,
        provider_refusal: false,
    };
    let mut evidence = release_evidence(record, &format!("{why}{}", candidate_note(&failure)));
    evidence.failure = Some(failure);
    ClaimMutation::Release(evidence)
}

pub(super) fn release_evidence(record: &LocalPullAdmission, why: &str) -> ClaimEvidence {
    let drain = &record.request.run_context.run_id;
    let machine = &record.destination.execution_machine_id;
    ClaimEvidence {
        summary: Some(format!("released by follower drain {drain}: {why}")),
        comment: Some(format!(
            "Follower drain {drain} on {machine} released this claim: {why}. The task is back \
             in the backlog and can be pulled again."
        )),
        ..Default::default()
    }
}

fn terminal_failure_summary_with(
    run: &orbit_types::workflow::JobRun,
    diagnostic: Option<(&str, &str)>,
) -> String {
    let mut summary = format!(
        "Outcome: failed\nClaimed leaf {} terminated as {} without an acknowledged typed handoff.",
        run.run_id, run.state
    );
    let failed_step = run
        .steps
        .iter()
        .rev()
        .find(|step| step.error_code.is_some() || step.error_message.is_some());
    if let Some(step) = failed_step {
        summary.push_str(&format!("\nFailed step: {}", step.target_id));
        if let Some(code) = step.error_code.as_deref() {
            summary.push_str(&format!(" ({code})"));
        }
        if let Some(message) = step
            .error_message
            .as_deref()
            .map(str::trim)
            .filter(|message| !message.is_empty())
        {
            let cut = floor_char_boundary(message, MAX_FAILURE_EXCERPT_BYTES);
            summary.push_str("\nError: ");
            summary.push_str(&message[..cut]);
            if cut < message.len() {
                summary.push_str(&format!(" [truncated to {cut} of {} bytes]", message.len()));
            }
        }
    } else if let Some((code, message)) = diagnostic {
        summary.push_str(&format!(
            "\nTerminal diagnostic ({code}): {}",
            message.trim()
        ));
    }
    summary.push_str(&format!(
        "\nInspect the run on its execution host: `orbit run show {}`.",
        run.run_id
    ));
    summary
}
