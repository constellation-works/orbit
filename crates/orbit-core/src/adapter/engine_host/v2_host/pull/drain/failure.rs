//! The settlement a terminal leaf implies, and the release evidence the owner
//! carries back to its task.
use orbit_common::text::floor_char_boundary;
use orbit_store::contracts::{
    ClaimEvidence, ClaimFailure, ClaimFinalRecovery, ClaimMutation, LocalPullAdmission,
    LocalPullPhase, ProviderUnavailable,
};
use orbit_types::workflow::{
    BASELINE_RED_MARKER, BaselineRedHold, ClaimFailureClass, FORGE_UNAVAILABLE_MARKER,
    FinalRecoveryDecision, ForgeUnavailableHold, JobRunState, OWNER_ROUTE_UNAVAILABLE_MARKER,
    PROVIDER_CAPACITY_MARKER, PROVIDER_LIMIT_MARKER, PROVIDER_UNAVAILABLE_MARKER, PipelineState,
    ReviewEvidenceHold, TRANSIENT_FAILURE_MARKER, UPGRADE_PENDING_MARKER,
    VALIDATION_ENVIRONMENT_MARKER, is_baseline_red_failure, is_forge_unavailable,
    is_provider_limit,
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
/// could not use [ORB-13941], a missing validation tool, an unreachable
/// owner, a red base, an inconclusive or interrupted run) releases it, and
/// the drain excludes the leaf's crew for its window when the class
/// [says so](ClaimFailureClass::excludes_crew).
///
/// Every follower process that settles a terminal leaf computes it here, so
/// the leaf's own worker, a cancel and a drain pass agree on the value.
/// `diagnostic` is the `(code, message)` the terminalizing caller knows before
/// its diagnostic step is durable; `state` is the leaf's pipeline state.
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
        return evidence_hold_release(record, run, hold);
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
        let hold = (failure.class == ClaimFailureClass::BaselineRed)
            .then(|| baseline_red(run, diagnostic))
            .flatten();
        // [ORB-14634] A leaf the forge kept refusing held its claim for its
        // push's retry window; the release says so and names the head.
        let forge = (failure.class == ClaimFailureClass::Transient
            && run.state == JobRunState::Held)
            .then(|| forge_hold(run, diagnostic, state))
            .flatten();
        let mut evidence = match (&hold, &forge) {
            (Some(hold), _) => baseline_red_release(record, run, hold, &failure),
            (None, Some(forge)) => forge_hold_release(record, run, forge, &failure),
            (None, None) => release_evidence(record, &release_reason(run, &failure)),
        };
        evidence.baseline_red = hold;
        evidence.forge_hold = forge;
        if failure.class == ClaimFailureClass::Provider {
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
/// state, under the `review_gate_settle` step's output.
fn held_evidence(state: Option<&PipelineState>) -> Option<ReviewEvidenceHold> {
    let hold = state?
        .pipeline
        .get("review_gate_settle")?
        .get("evidence_hold")?;
    serde_json::from_value(hold.clone()).ok()
}

/// A `held` leaf whose review settled into an evidence hold did not fail: its
/// claim is released with the typed hold, which the owner records as the
/// task's latest decision.
fn evidence_hold_release(
    record: &LocalPullAdmission,
    run: &orbit_types::workflow::JobRun,
    hold: ReviewEvidenceHold,
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
    ClaimMutation::Release(ClaimEvidence {
        summary: Some(format!("released by follower drain {drain}: {why}")),
        comment: Some(format!(
            "Follower drain {drain} on {machine} released this claim: {why}. The task stays in \
             progress under the evidence hold; attaching every named result queues a fresh \
             review. No pull request was opened."
        )),
        evidence_hold: Some(hold),
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
    // before the reason drops it.
    let provider_limit = typed.as_ref().is_some_and(|(class, reason)| {
        *class == ClaimFailureClass::Provider && is_provider_limit(None, Some(reason))
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

/// The forge hold a held leaf ended on: the one its run state records, else
/// the one its held step, or the terminalizing caller's diagnostic, carries.
fn forge_hold(
    run: &orbit_types::workflow::JobRun,
    diagnostic: Option<(&str, &str)>,
    state: Option<&PipelineState>,
) -> Option<ForgeUnavailableHold> {
    state
        .and_then(|state| state.forge_hold.clone())
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
    } else if failure.class.excludes_crew() {
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
pub(crate) fn operator_cancel_release(record: &LocalPullAdmission, why: &str) -> ClaimMutation {
    let mut evidence = release_evidence(record, why);
    evidence.failure = Some(ClaimFailure {
        class: ClaimFailureClass::OperatorCancel,
        reason: why.to_string(),
        crew: record
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.task.as_ref())
            .and_then(|task| task.crew.clone()),
        candidate: None,
        provider_limit: false,
    });
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
