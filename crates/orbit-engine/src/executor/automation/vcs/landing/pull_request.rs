//! Landing a handoff through its pull request: check waits, recovery and the merge.

use std::time::Duration;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::context::{HandoffLandingStep, RuntimeHost};

use super::super::operations;
use super::super::pr::diagnostics;
use super::super::pr::{
    BlockedCheckWait, OwedHostedChecks, PrMergeState, UnreportedChecks, checked_pr_state,
    classify_pr_state, owed_hosted_checks, read_pr_status, read_pr_status_before,
    resolve_permitted_merge,
};
use super::super::remote_probe::fixed_wait as sleep;
use super::checks::{CheckRecovery, CheckRecoveryStep, failed_checks, recover_failed_checks};
use super::intent::LandingView;
use super::observe::{adopt_base_update, landing_tip, observe_candidate, pin_for};
use super::record::{behind_base, complete, describe, record, refuse_pr, stop_pr};
use super::run::{LandingBudget, LandingPass, transport_failure};

pub(super) fn land_pull_request<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    number: u64,
    budget: &LandingBudget,
    view: &LandingView,
) -> Result<Value, OrbitError> {
    let candidate = &context.candidate;
    let pr_number = number.to_string();
    let workspace = context.workspace_path.clone();
    let workspace_path = workspace.to_string_lossy().into_owned();
    let mut pin = pin_for(candidate);

    let LandingBudget {
        max_wait_seconds,
        poll_interval_seconds,
        started,
        deadline_ms,
    } = *budget;
    let mut recovery = CheckRecovery::before(deadline_ms);
    let mut unreported = UnreportedChecks::new();
    let mut waited_seconds = 0_u64;
    let mut intent: Option<String> = None;
    let mut diagnostic = json!({"pr":{"state":"not_collected"},
        "required_checks":{"state":"not_collected"}, "status_observations":0,
        "classification":{"state":"not_collected"}});
    loop {
        if max_wait_seconds > 0
            && (waited_seconds >= max_wait_seconds
                || super::super::completion_deadline::now_ms()? >= deadline_ms)
        {
            return Err(landing_timeout(
                host,
                context,
                number,
                max_wait_seconds,
                &context.owed_hosted_checks,
                &unreported,
                &diagnostic,
            )?);
        }
        let status = if max_wait_seconds == 0 {
            read_pr_status(host, &workspace, &pr_number)?
        } else {
            read_pr_status_before(host, &workspace, &pr_number, deadline_ms)?
        };
        diagnostic["pr"] = diagnostics::status(&status);
        diagnostic["required_checks"] = json!({"state":"not_collected"});
        diagnostic["unreported_wait"] = json!({"state":"not_collected"});
        diagnostic["status_observations"] = json!(
            diagnostic["status_observations"]
                .as_u64()
                .unwrap_or(0)
                .saturating_add(1)
        );
        diagnostic["status_history_truncated"] =
            json!(diagnostic["status_observations"].as_u64().unwrap_or(0) > 1);
        let mut state = classify_pr_state(&status);
        diagnostic["classification"] = diagnostics::text(&describe(&state));
        waited_seconds = waited_seconds.max(started.elapsed().as_secs());
        // [ORB-15956] A branch update this landing (or an earlier attempt)
        // requested moved the head onto a verified merge with the base.
        let updated_head = adopt_base_update(&mut pin, context, &status);
        let refresh = super::super::base::refresh::recover(
            host,
            &workspace,
            std::slice::from_ref(&context.task_id),
            &context.handoff_id,
            candidate,
            &status,
            deadline_ms,
            false,
            Some(&context.handoff_id),
        )
        .map_err(|error| {
            stop_pr(host, context, &diagnostic, false, &error.to_string()).unwrap_or(error)
        })?;
        if matches!(refresh, super::super::base::refresh::RefreshStep::Changed) {
            continue;
        }
        let mut refresh_wait = matches!(
            refresh,
            super::super::base::refresh::RefreshStep::Wait
                | super::super::base::refresh::RefreshStep::Adopted { waiting: true }
        );
        if let PrMergeState::Merged = state {
            let delivered = match pin.ensure_delivered(&status, &pr_number) {
                Ok(delivered) => delivered,
                // A merged pull request whose identity is not ours is not this
                // handoff's delivery, whatever it merged.
                Err(error) => {
                    return Err(stop_pr(
                        host,
                        context,
                        &diagnostic,
                        false,
                        &error.to_string(),
                    )?);
                }
            };
            let mut evidence = json!({
                "delivery": delivered.as_json(),
                "pull_request": number,
                "candidate": candidate.candidate.commit,
            });
            if let Some(head) = &updated_head {
                evidence["updated_head"] = json!(head);
            }
            if let Some(actions) = recovery.evidence() {
                evidence["check_recovery"] = actions;
            }
            let observed = observe_candidate(context, Some(&status), view).map_err(|error| {
                stop_pr(host, context, &diagnostic, false, &error.to_string()).unwrap_or(error)
            })?;
            if let Some(intent_id) = intent {
                record(
                    host,
                    context,
                    HandoffLandingStep::ResolveIntent {
                        intent_id,
                        merged: true,
                    },
                    Some(observed.clone()),
                    &evidence,
                )?;
            }
            return complete(host, context, observed, &evidence);
        }

        // Every non-merged state below is read against the pinned identity
        // first: a repointed branch or base must never receive a merge request.
        if let Err(error) = pin.ensure_pinned_candidate(&status, &pr_number) {
            return Err(stop_pr(
                host,
                context,
                &diagnostic,
                false,
                &error.to_string(),
            )?);
        }
        let checked = checked_pr_state(
            host,
            &workspace,
            &pr_number,
            &status,
            BlockedCheckWait::Owner(&context.owed_hosted_checks),
            &mut unreported,
            deadline_ms,
        );
        diagnostic["required_checks"] = unreported.required_observation.clone();
        diagnostic["unreported_wait"] = unreported.diagnostic();
        state = match checked {
            Ok(state) => state,
            Err(reason) => {
                let message = format!("pull request #{pr_number} cannot be merged ({reason})");
                let error = reason
                    .into_error(|_| OrbitError::Execution(format!("handoff_land: {message}")));
                // Keep the provider's transport class until the outer loop
                // applies the shared deadline and the external-write fence.
                if transport_failure(&error) {
                    return Err(error);
                }
                diagnostic["classification"] = diagnostics::text("required_check_refusal");
                return Err(stop_pr(host, context, &diagnostic, false, &message)?);
            }
        };
        diagnostic["classification"] = diagnostics::text(&describe(&state));
        // A required context the head has not reported may be one only the
        // advanced base emits, so absent-context recovery still diagnoses it.
        if !refresh_wait
            && (matches!(state, PrMergeState::Blocked(_)) || unreported.awaiting_policy_checks())
        {
            match super::super::base::refresh::recover(
                host,
                &workspace,
                std::slice::from_ref(&context.task_id),
                &context.handoff_id,
                candidate,
                &status,
                deadline_ms,
                true,
                Some(&context.handoff_id),
            )
            .map_err(|error| {
                stop_pr(host, context, &diagnostic, false, &error.to_string()).unwrap_or(error)
            })? {
                super::super::base::refresh::RefreshStep::Changed => continue,
                super::super::base::refresh::RefreshStep::Wait
                | super::super::base::refresh::RefreshStep::Adopted { waiting: true } => {
                    refresh_wait = true
                }
                _ => {}
            }
        }
        waited_seconds = waited_seconds.max(started.elapsed().as_secs());
        // [ORB-15677] [ORB-15812] The review owed these checks to the pull
        // request's hosted CI instead of running them: nothing merges until
        // each reports an explicit success on the pinned head. A skipped or
        // neutral result keeps waiting. Branch protection is separate.
        let owed = owed_hosted_checks(&status, &context.owed_hosted_checks);
        // [ORB-15956] A failed check, owed or required, is classified and
        // recovered before anything stops on it.
        let failed = failed_checks(&status);
        let check_reason = match (&owed, &state) {
            _ if matches!(
                state,
                PrMergeState::Closed | PrMergeState::Contradictory(_) | PrMergeState::Conflict
            ) || behind_base(&status)
                || status.is_draft
                || status.merge_state_status.as_deref() == Some("DRAFT") =>
            {
                None
            }
            (OwedHostedChecks::Failed(reason), _) => Some(format!(
                "pull request #{pr_number}: {reason}; the review owed it to that check"
            )),
            (_, PrMergeState::Blocked(reason)) if !failed.is_empty() && !behind_base(&status) => {
                Some(format!(
                    "pull request #{pr_number} cannot be merged ({reason})"
                ))
            }
            _ => None,
        };
        let mut waiting_on_recovery = refresh_wait;
        if let Some(reason) = check_reason {
            if failed.is_empty() {
                return Err(stop_pr(
                    host,
                    context,
                    &diagnostic,
                    false,
                    &format!("{reason}, so the candidate stays in review"),
                )?);
            }
            let tip = landing_tip(context, view);
            // A rerun or branch update is an external write, and one that
            // failed may have been applied, so neither retries the pass.
            let recovered = recover_failed_checks(
                host,
                context,
                &status,
                &failed,
                tip.as_deref(),
                &mut recovery,
                &reason,
            )
            .inspect_err(|_| context.wrote.set(true))?;
            if matches!(recovered, CheckRecoveryStep::Acted) {
                context.wrote.set(true);
            }
            if let Some(observation) = recovery.required_observation.take() {
                diagnostic["required_checks"] = observation;
            }
            match recovered {
                CheckRecoveryStep::Acted => continue,
                CheckRecoveryStep::Wait => waiting_on_recovery = true,
                CheckRecoveryStep::Repair(evidence) => {
                    return Err(stop_pr(host, context, &diagnostic, true, &evidence)?);
                }
                CheckRecoveryStep::Requeue(evidence) => {
                    return Err(refuse_pr(
                        host,
                        context,
                        &diagnostic,
                        HandoffLandingStep::StopForRequeue,
                        &evidence,
                    )?);
                }
                CheckRecoveryStep::Stop(evidence) => {
                    return Err(stop_pr(host, context, &diagnostic, false, &evidence)?);
                }
            }
        }
        match state {
            _ if waiting_on_recovery => {}
            PrMergeState::Merged => unreachable!("merged is handled above"),
            PrMergeState::Closed => {
                return Err(stop_pr(
                    host,
                    context,
                    &diagnostic,
                    false,
                    &format!(
                        "pull request #{pr_number} was closed without merging; the candidate \
                         stays in review"
                    ),
                )?);
            }
            PrMergeState::Contradictory(reason) => {
                return Err(stop_pr(
                    host,
                    context,
                    &diagnostic,
                    false,
                    &format!(
                        "pull request #{pr_number} reports a contradictory merge state \
                         ({reason}); landing needs an unambiguous merged state"
                    ),
                )?);
            }
            PrMergeState::Blocked(reason) => {
                let reason = format!(
                    "pull request #{pr_number} cannot be merged ({reason}); branch \
                     protection and required checks are not bypassed"
                );
                // A branch that is only behind its base is stale, not refused:
                // the repair brings it up to date and revalidates it.
                return Err(if behind_base(&status) {
                    stop_pr(host, context, &diagnostic, true, &reason)?
                } else {
                    stop_pr(host, context, &diagnostic, false, &reason)?
                });
            }
            PrMergeState::Conflict => {
                return Err(stop_pr(
                    host,
                    context,
                    &diagnostic,
                    true,
                    &format!(
                        "pull request #{pr_number} conflicts with its base; the owner does not \
                         rebase unvalidated code, so a repair needs fresh validation and a new \
                         handoff"
                    ),
                )?);
            }
            PrMergeState::Mergeable if intent.is_none() && owed == OwedHostedChecks::Passed => {
                if max_wait_seconds > 0
                    && super::super::completion_deadline::now_ms()? >= deadline_ms
                {
                    return Err(landing_timeout(
                        host,
                        context,
                        number,
                        max_wait_seconds,
                        &context.owed_hosted_checks,
                        &unreported,
                        &diagnostic,
                    )?);
                }
                if updated_head.is_some() {
                    let current = host.run_private_vcs_operation(operations::CI_CURRENT_GATES, json!({"workspace_path":workspace,"pr":pr_number,"head_sha":status.head_sha,"head_branch":status.head_branch,"base_branch":status.base_branch,"deadline_ms":deadline_ms}))
                        .map_err(|error| stop_pr(host, context, &diagnostic, false,&error.to_string()).unwrap_or(error))?;
                    if current["gates_passed"] != true {
                        // A newly required context that has not succeeded is
                        // still a gate, including on a provider-reported CLEAN.
                        if max_wait_seconds == 0 {
                            return Err(landing_timeout(
                                host,
                                context,
                                number,
                                max_wait_seconds,
                                &context.owed_hosted_checks,
                                &unreported,
                                &diagnostic,
                            )?);
                        }
                        let delay = poll_interval_seconds
                            .min(max_wait_seconds.saturating_sub(waited_seconds));
                        sleep(Duration::from_secs(delay));
                        waited_seconds = waited_seconds.saturating_add(delay);
                        continue;
                    }
                }
                let capabilities = resolve_permitted_merge(host, &workspace_path, &pr_number)
                    .map_err(|error| {
                        operations::carry_class(
                            &error,
                            OrbitError::Execution(format!(
                                "handoff_land: no permitted merge method for pull request \
                                 #{pr_number}: {error}"
                            )),
                        )
                    })?;
                if max_wait_seconds > 0
                    && super::super::completion_deadline::now_ms()? >= deadline_ms
                {
                    return Err(landing_timeout(
                        host,
                        context,
                        number,
                        max_wait_seconds,
                        &context.owed_hosted_checks,
                        &unreported,
                        &diagnostic,
                    )?);
                }
                let observed =
                    observe_candidate(context, Some(&status), view).map_err(|error| {
                        stop_pr(host, context, &diagnostic, false, &error.to_string())
                            .unwrap_or(error)
                    })?;
                if observed.repository != capabilities.repository {
                    return Err(stop_pr(
                        host,
                        context,
                        &diagnostic,
                        false,
                        &format!(
                            "pull request #{pr_number} belongs to repository \
                             '{}', not the handoff's '{}'",
                            capabilities.repository, candidate.repository
                        ),
                    )?);
                }
                let intent_id = format!("{}:{pr_number}", context.handoff_id);
                // Durable before the external mutation: a lost reply after this
                // point is uncertainty the next attempt reconciles.
                let head = pin
                    .candidate_sha()
                    .unwrap_or(&candidate.candidate.commit)
                    .to_string();
                record(
                    host,
                    context,
                    HandoffLandingStep::PublishIntent {
                        intent_id: intent_id.clone(),
                    },
                    Some(observed),
                    &json!({
                        "sending": "pull_request_merge",
                        "pull_request": number,
                        "strategy": capabilities.strategy.as_str(),
                        "candidate": candidate.candidate.commit,
                        "head": head,
                    }),
                )?;
                intent = Some(intent_id);
                // The synchronous mutation carries the pinned head, so the
                // provider itself refuses a candidate that moved.
                host.run_private_vcs_operation(
                    operations::PR_MERGE,
                    json!({
                        "pr": pr_number,
                        "strategy": capabilities.strategy.as_str(),
                        "auto": false,
                        "reviewed_head_sha": head,
                        "workspace_path": workspace_path,
                    }),
                )?;
                // Merged is what the provider reports, not what we asked for.
                continue;
            }
            // Requested already, or still waiting for required or owed checks.
            PrMergeState::Mergeable | PrMergeState::Pending => {}
        }

        if waited_seconds >= max_wait_seconds {
            let waiting = match &owed {
                OwedHostedChecks::Waiting(names) if intent.is_none() => names.as_slice(),
                _ => &[],
            };
            return Err(landing_timeout(
                host,
                context,
                number,
                max_wait_seconds,
                waiting,
                &unreported,
                &diagnostic,
            )?);
        }
        let sleep_seconds = poll_interval_seconds.min(max_wait_seconds - waited_seconds);
        sleep(Duration::from_secs(sleep_seconds));
        waited_seconds = waited_seconds.saturating_add(sleep_seconds);
    }
}

fn landing_timeout<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    number: u64,
    max_wait_seconds: u64,
    owed: &[String],
    unreported: &UnreportedChecks,
    diagnostic: &Value,
) -> Result<OrbitError, OrbitError> {
    let mut waiting = if owed.is_empty() {
        String::new()
    } else {
        format!(" (owed hosted checks not yet passed: {})", owed.join(", "))
    };
    waiting.push_str(&unreported.timeout_note());
    stop_pr(
        host,
        context,
        diagnostic,
        false,
        &format!(
            "pull request #{number} did not reach a merged state within {max_wait_seconds}s{waiting}; the candidate stays in review"
        ),
    )
}
