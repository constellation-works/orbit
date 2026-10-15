//! The owner's landing step for an authorized handoff [ORB-12499].
//!
//! A follower executes, validates and publishes a candidate; it never merges.
//! The owner's durable landing job runs this activity, which is the only place
//! the external merge actually happens. Its contract with the owner store is:
//!
//! 1. **Observe, never assume.** Every fact this activity reports — the pull
//!    request's head, base and merged state, the candidate and base objects,
//!    the local landing ref — is read from the provider or the owner checkout
//!    here. Nothing is copied out of the worker's handoff payload, and a fact
//!    that cannot be read is a refusal rather than a pass. Fields the owner
//!    holds but cannot independently observe (the repository name on a
//!    checkout without a remote) are carried from the accepted candidate and
//!    named as such in the evidence.
//! 2. **Intent before the call.** A merge intent is persisted before the first
//!    external mutation, so a lost reply or a crash leaves recorded
//!    uncertainty. The next attempt reconciles that intent against the
//!    provider's actual state (or the local target ref) before it retries
//!    anything, and the owner store refuses reassignment until it does.
//! 3. **Stop rather than repair.** A changed head or base, a conflict, an
//!    unsatisfied branch protection or a check budget that runs out stops the
//!    attempt with durable evidence. Repairing the candidate needs fresh
//!    validation and a new handoff; this activity never rebases, force-pushes
//!    or merges around a gate. A stop on a conflicting or stale base is
//!    recorded as repairable, and the owner store hands that candidate to one
//!    automatic repair leaf that does exactly that [ORB-14261]. A failed
//!    hosted check is classified first ([`super::landing_checks`]): a failure
//!    inherited from the base updates the branch onto the current base once,
//!    a transient one is rerun once, and one the candidate caused stops for
//!    the same single repair [ORB-15956].
//!
//! The guarded `review -> done` transition is the owner store's, behind its own
//! recheck of the current authorization and the digest-pinned validation
//! evidence. This activity only supplies the merge evidence that permits it.

use std::path::Path;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_types::workflow::automation::{DirectLandingRequest, SourceRevision};
use orbit_types::workflow::handoff::{HandoffCandidate, HandoffDelivery};
use serde_json::{Value, json};

use crate::context::{
    HandoffLandingContext, HandoffLandingStep, HandoffLandingUpdate, RuntimeHost,
};

use super::super::ci::bounded_u64;
use super::super::input::input_string_field;
use super::git::{fetch_remote_base, git_command_success, git_output};
use super::landing_checks::{
    CheckRecovery, CheckRecoveryStep, failed_checks, recover_failed_checks, verified_base_update,
};
use super::operations;
use super::pr::{
    BlockedCheckWait, DeliveryPin, OwedHostedChecks, PrMergeState, PullRequestStatus,
    checked_pr_state, classify_pr_state, owed_hosted_checks, read_pr_status, read_pr_status_before,
    resolve_permitted_merge,
};
use super::remote_probe::fixed_wait as sleep;
use super::worktree::{checkout_holding_branch, ensure_clean_checkout};

/// Budget for waiting out required checks on a pull request before the attempt
/// stops. The same shape `pr_complete` uses, so operators tune one thing.
const DEFAULT_MAX_WAIT_SECONDS: u64 = 3600;
const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 30;
const MAX_WAIT_SECONDS: u64 = 6 * 60 * 60;
const MIN_POLL_INTERVAL_SECONDS: u64 = 5;
const MAX_POLL_INTERVAL_SECONDS: u64 = 10 * 60;

pub(in crate::executor::automation) fn handoff_land<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let handoff_id = input_string_field(input, "handoff_id").ok_or_else(|| {
        OrbitError::InvalidInput("handoff_land: handoff_id is required".to_string())
    })?;
    let context = host.handoff_landing_context(&handoff_id)?;
    // Every containment judgement below reads the landing ref, so the owner's
    // view of it is refreshed once before any of them run.
    let view = LandingView::refresh(
        &context.workspace_path,
        &context.candidate.landing_branch,
        &context.candidate.delivery,
    );

    // An intent recorded but never resolved is this owner's own uncertainty
    // about an external write. Nothing else may happen until real state says
    // what became of it.
    if let Some(intent_id) = context.unresolved_merge_intent.clone()
        && let Some(landed) = reconcile_intent(host, &context, &intent_id, &view)?
    {
        return Ok(landed);
    }

    match context.candidate.delivery.clone() {
        HandoffDelivery::PullRequest { number } => {
            land_pull_request(host, &context, number, input, &view)
        }
        // [ORB-15677] Only a pull request's hosted CI runs the checks the
        // review owed it; a local candidate has none to wait for.
        HandoffDelivery::LocalCandidate if !context.owed_hosted_checks.is_empty() => Err(stop(
            host,
            &context,
            &format!(
                "the review owed hosted checks ({}) to a pull request, but the candidate is \
                 delivered locally; the candidate stays in review",
                context.owed_hosted_checks.join(", ")
            ),
        )?),
        HandoffDelivery::LocalCandidate => land_local_candidate(host, &context, input, &view),
        HandoffDelivery::NoDiff { .. } => land_no_diff(host, &context, &view),
        HandoffDelivery::AlreadyLanded {
            covering_commit, ..
        } => land_already_landed(host, &context, &covering_commit, &view),
    }
}

/// The owner's view of the landing branch, refreshed once per attempt.
///
/// Containment is judged against `origin/<landing branch>`, a remote-tracking
/// ref that only moves when this checkout fetches it. A merge another consumer
/// performed on the provider, or a covering commit another machine landed,
/// leaves that ref behind the real branch — and judging against it unrefreshed
/// turns ordinary freshness into a durable refusal whose evidence says the
/// opposite of what is true [ORB-12631]. Fetching the branch also brings the
/// objects reachable from it, which is how a base or covering commit the owner
/// has never seen becomes readable here.
struct LandingView {
    refreshed: bool,
}

impl LandingView {
    /// Fetch the landing branch from `origin`, best effort.
    ///
    /// Owner-local delivery has no remote to read, and a fetch that cannot run
    /// (no `origin`, no network) leaves the judgement to the refs already
    /// present rather than replacing a freshness problem with a different
    /// failure: what the owner still cannot verify is refused below either way.
    /// Adopting a published PR's updated head requires its own successful
    /// fetch, even when this initial refresh failed.
    fn refresh(workspace: &Path, landing_branch: &str, delivery: &HandoffDelivery) -> Self {
        if matches!(delivery, HandoffDelivery::LocalCandidate)
            || !git_command_success(workspace, &["remote", "get-url", "origin"]).unwrap_or(false)
        {
            return Self { refreshed: false };
        }
        match fetch_remote_base(workspace, landing_branch) {
            Ok(()) => Self { refreshed: true },
            Err(error) => {
                tracing::warn!(
                    %error,
                    landing_branch,
                    "handoff_land: landing ref not refreshed; containment is judged against the \
                     owner's existing view"
                );
                Self { refreshed: false }
            }
        }
    }

    /// Names the state a refusal was decided against, so an operator reading
    /// "no longer reachable" knows whether the owner's view was current.
    fn clause(&self) -> &'static str {
        if self.refreshed {
            " (refreshed from 'origin')"
        } else {
            ""
        }
    }
}

/// Read what actually became of an unresolved merge intent and record it.
///
/// Returns the completion outcome when the external state proves the candidate
/// merged; `None` means the intent is resolved as *not* merged and this run may
/// attempt a fresh, fully rechecked landing. A merged pull request with a
/// different identity resolves the external uncertainty before recording Stop;
/// resolving an intent is not proof that the authorized candidate landed.
fn reconcile_intent<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    intent_id: &str,
    view: &LandingView,
) -> Result<Option<Value>, OrbitError> {
    let candidate = &context.candidate;
    let mut delivery_error = None;
    let (merged, evidence) = match &candidate.delivery {
        HandoffDelivery::PullRequest { number } => {
            let status = read_pr_status(host, &context.workspace_path, &number.to_string())?;
            match classify_pr_state(&status) {
                PrMergeState::Merged => {
                    let mut pin = pin_for(candidate);
                    adopt_base_update(&mut pin, context, &status);
                    let evidence = match pin.ensure_delivered(&status, &number.to_string()) {
                        Ok(delivered) => json!({
                            "reconciled": "pull_request_merged",
                            "pull_request": number,
                            "delivery": delivered.as_json(),
                        }),
                        Err(error) => {
                            let evidence = json!({
                                "reconciled": "pull_request_merged",
                                "pull_request": number,
                                "provider_state": status.provider_state(),
                                "delivery_error": error.to_string(),
                            });
                            delivery_error = Some(error);
                            evidence
                        }
                    };
                    (true, evidence)
                }
                other => (
                    false,
                    json!({
                        "reconciled": "pull_request_open",
                        "pull_request": number,
                        "provider_state": describe(&other),
                    }),
                ),
            }
        }
        HandoffDelivery::LocalCandidate => {
            let landing = resolve_landing_ref(
                &context.workspace_path,
                &candidate.landing_branch,
                &candidate.delivery,
            )?;
            let merged = contains_commit(
                &context.workspace_path,
                &landing,
                &candidate.candidate.commit,
            )?;
            (
                merged,
                json!({
                    "reconciled": if merged { "local_target_contains_candidate" } else { "local_target_unchanged" },
                    "landing_ref": landing,
                    "candidate": candidate.candidate.commit,
                }),
            )
        }
        // No-diff delivery performs no external call, so it never publishes an
        // intent; an intent recorded against one is not something this activity
        // may resolve by guessing.
        HandoffDelivery::AlreadyLanded { .. } | HandoffDelivery::NoDiff { .. } => {
            return Err(OrbitError::Execution(format!(
                "handoff_land: handoff '{}' records an unresolved merge intent for no-diff \
                 delivery, which performs no external merge; reconcile it explicitly",
                context.handoff_id
            )));
        }
    };

    // Resolving records what was read, not an authority decision, so it needs
    // no candidate observation — and clearing the uncertainty first is what
    // lets a stop or a fresh attempt happen at all.
    record(
        host,
        context,
        HandoffLandingStep::ResolveIntent {
            intent_id: intent_id.to_string(),
            merged,
        },
        None,
        &evidence,
    )?;
    if let Some(error) = delivery_error {
        return Err(stop(host, context, &error.to_string())?);
    }
    if !merged {
        return Ok(None);
    }
    let observed = observe_or_stop(host, context, None, view)?;
    Ok(Some(complete(host, context, observed, &evidence)?))
}

fn land_pull_request<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    number: u64,
    input: &Value,
    view: &LandingView,
) -> Result<Value, OrbitError> {
    let candidate = &context.candidate;
    let pr_number = number.to_string();
    let workspace = context.workspace_path.clone();
    let workspace_path = workspace.to_string_lossy().into_owned();
    let mut pin = pin_for(candidate);
    let mut recovery = CheckRecovery::default();

    let max_wait_seconds = bounded_u64(
        input,
        "max_wait_seconds",
        DEFAULT_MAX_WAIT_SECONDS,
        MAX_WAIT_SECONDS,
    )?;
    let poll_interval_seconds = bounded_u64(
        input,
        "poll_interval_seconds",
        DEFAULT_POLL_INTERVAL_SECONDS,
        MAX_POLL_INTERVAL_SECONDS,
    )?
    .max(MIN_POLL_INTERVAL_SECONDS);

    let started = Instant::now();
    let deadline_ms =
        super::completion_deadline::now_ms()?.saturating_add(max_wait_seconds.saturating_mul(1000));
    let mut waited_seconds = 0_u64;
    let mut intent: Option<String> = None;
    loop {
        if max_wait_seconds > 0
            && (waited_seconds >= max_wait_seconds
                || super::completion_deadline::now_ms()? >= deadline_ms)
        {
            return Err(landing_timeout(
                host,
                context,
                number,
                max_wait_seconds,
                &context.owed_hosted_checks,
            )?);
        }
        let status = if max_wait_seconds == 0 {
            read_pr_status(host, &workspace, &pr_number)?
        } else {
            read_pr_status_before(host, &workspace, &pr_number, deadline_ms)?
        };
        let mut state = classify_pr_state(&status);
        waited_seconds = waited_seconds.max(started.elapsed().as_secs());
        // [ORB-15956] A branch update this landing (or an earlier attempt)
        // requested moved the head onto a verified merge with the base.
        let updated_head = adopt_base_update(&mut pin, context, &status);
        if let PrMergeState::Merged = state {
            let delivered = match pin.ensure_delivered(&status, &pr_number) {
                Ok(delivered) => delivered,
                // A merged pull request whose identity is not ours is not this
                // handoff's delivery, whatever it merged.
                Err(error) => return Err(stop(host, context, &error.to_string())?),
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
            let observed = observe_or_stop(host, context, Some(&status), view)?;
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
            return Err(stop(host, context, &error.to_string())?);
        }
        state = match checked_pr_state(
            host,
            &workspace,
            &pr_number,
            &status,
            BlockedCheckWait::Owner(&context.owed_hosted_checks),
            deadline_ms,
        ) {
            Ok(state) => state,
            Err(reason) => {
                return Err(stop(
                    host,
                    context,
                    &format!("pull request #{pr_number} cannot be merged ({reason})"),
                )?);
            }
        };
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
        let mut waiting_on_recovery = false;
        if let Some(reason) = check_reason {
            if failed.is_empty() {
                return Err(stop(
                    host,
                    context,
                    &format!("{reason}, so the candidate stays in review"),
                )?);
            }
            let tip = landing_tip(context, view);
            match recover_failed_checks(
                host,
                context,
                &status,
                &failed,
                tip.as_deref(),
                &mut recovery,
                &reason,
            )? {
                CheckRecoveryStep::Acted => continue,
                CheckRecoveryStep::Wait => waiting_on_recovery = true,
                CheckRecoveryStep::Repair(evidence) => {
                    return Err(stop_for_repair(host, context, &evidence)?);
                }
                CheckRecoveryStep::Stop(evidence) => {
                    return Err(stop(host, context, &evidence)?);
                }
            }
        }
        match state {
            _ if waiting_on_recovery => {}
            PrMergeState::Merged => unreachable!("merged is handled above"),
            PrMergeState::Closed => {
                return Err(stop(
                    host,
                    context,
                    &format!(
                        "pull request #{pr_number} was closed without merging; the candidate \
                         stays in review"
                    ),
                )?);
            }
            PrMergeState::Contradictory(reason) => {
                return Err(stop(
                    host,
                    context,
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
                    stop_for_repair(host, context, &reason)?
                } else {
                    stop(host, context, &reason)?
                });
            }
            PrMergeState::Conflict => {
                return Err(stop_for_repair(
                    host,
                    context,
                    &format!(
                        "pull request #{pr_number} conflicts with its base; the owner does not \
                         rebase unvalidated code, so a repair needs fresh validation and a new \
                         handoff"
                    ),
                )?);
            }
            PrMergeState::Mergeable if intent.is_none() && owed == OwedHostedChecks::Passed => {
                if max_wait_seconds > 0 && super::completion_deadline::now_ms()? >= deadline_ms {
                    return Err(landing_timeout(
                        host,
                        context,
                        number,
                        max_wait_seconds,
                        &context.owed_hosted_checks,
                    )?);
                }
                let capabilities = resolve_permitted_merge(host, &workspace_path, &pr_number)
                    .map_err(|error| {
                        OrbitError::Execution(format!(
                            "handoff_land: no permitted merge method for pull request \
                                 #{pr_number}: {error}"
                        ))
                    })?;
                if max_wait_seconds > 0 && super::completion_deadline::now_ms()? >= deadline_ms {
                    return Err(landing_timeout(
                        host,
                        context,
                        number,
                        max_wait_seconds,
                        &context.owed_hosted_checks,
                    )?);
                }
                let observed = observe_or_stop(host, context, Some(&status), view)?;
                if observed.repository != capabilities.repository {
                    return Err(stop(
                        host,
                        context,
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
            )?);
        }
        let sleep_seconds = poll_interval_seconds.min(max_wait_seconds - waited_seconds);
        sleep(Duration::from_secs(sleep_seconds));
        waited_seconds = waited_seconds.saturating_add(sleep_seconds);
    }
}

fn landing_timeout<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    number: u64,
    max_wait_seconds: u64,
    owed: &[String],
) -> Result<OrbitError, OrbitError> {
    let waiting = if owed.is_empty() {
        String::new()
    } else {
        format!(" (owed hosted checks not yet passed: {})", owed.join(", "))
    };
    stop(
        host,
        context,
        &format!(
            "pull request #{number} did not reach a merged state within {max_wait_seconds}s{waiting}; the candidate stays in review"
        ),
    )
}

/// Owner-local delivery: no origin, no pull request, and a fast-forward of the
/// landing branch is the entire external effect.
fn land_local_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    input: &Value,
    view: &LandingView,
) -> Result<Value, OrbitError> {
    let candidate = &context.candidate;
    let workspace = context.workspace_path.clone();
    let observed = observe_or_stop(host, context, None, view)?;
    let landing = resolve_landing_ref(&workspace, &candidate.landing_branch, &candidate.delivery)?;

    if contains_commit(&workspace, &landing, &candidate.candidate.commit)? {
        let evidence = json!({
            "delivery": "local_candidate",
            "landing_ref": landing,
            "candidate": candidate.candidate.commit,
            "verified": "landing_ref_contains_candidate",
        });
        return complete(host, context, observed, &evidence);
    }

    let checkout = checkout_holding_branch(&workspace, &candidate.landing_branch)?
        .unwrap_or_else(|| workspace.clone());
    ensure_clean_checkout(&checkout, "owner landing checkout")?;
    let checked_out = git_output(&checkout, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if checked_out.trim() != candidate.landing_branch {
        return Err(stop(
            host,
            context,
            &format!(
                "owner checkout '{}' has '{}' checked out, not landing branch '{}'",
                checkout.display(),
                checked_out.trim(),
                candidate.landing_branch
            ),
        )?);
    }

    // A fast-forward is a direct landing with no provider identity, so the
    // owner's delivery consumers learn which task it delivered only from an
    // intent retained before the branch moves [ORB-13894]. A candidate that
    // cannot fast-forward lands nothing and stops below.
    let before = git_output(&checkout, &["rev-parse", "HEAD"])?;
    if git_command_success(
        &checkout,
        &[
            "merge-base",
            "--is-ancestor",
            before.trim(),
            &candidate.candidate.commit,
        ],
    )? {
        let run_id = input_string_field(input, "run_id").ok_or_else(|| {
            OrbitError::InvalidInput("handoff_land: run_id is required".to_string())
        })?;
        host.record_direct_landing_intent(&DirectLandingRequest {
            run_id,
            branch: candidate.landing_branch.clone(),
            before_commit: before.trim().to_string(),
            after_commit: candidate.candidate.commit.clone(),
            task_ids: vec![context.task_id.clone()],
            handoff_id: Some(context.handoff_id.clone()),
        })?;
    }

    let intent_id = format!("{}:local", context.handoff_id);
    record(
        host,
        context,
        HandoffLandingStep::PublishIntent {
            intent_id: intent_id.clone(),
        },
        Some(observed.clone()),
        &json!({
            "sending": "local_fast_forward",
            "landing_branch": candidate.landing_branch,
            "candidate": candidate.candidate.commit,
        }),
    )?;
    let fast_forwarded = git_command_success(
        &checkout,
        &["merge", "--ff-only", &candidate.candidate.commit],
    )?;
    // The ref itself is the evidence either way: a failed fast-forward left it
    // where it was, which is a resolved intent rather than an uncertain one.
    let merged = contains_commit(&workspace, &landing, &candidate.candidate.commit)?;
    let evidence = json!({
        "delivery": "local_candidate",
        "landing_ref": landing,
        "candidate": candidate.candidate.commit,
        "fast_forwarded": fast_forwarded,
        "verified": if merged { "landing_ref_contains_candidate" } else { "landing_ref_unchanged" },
    });
    record(
        host,
        context,
        HandoffLandingStep::ResolveIntent { intent_id, merged },
        Some(observed.clone()),
        &evidence,
    )?;
    if !merged {
        return Err(stop_for_repair(
            host,
            context,
            &format!(
                "landing branch '{}' cannot fast-forward to candidate {}; the owner does not \
                 rebase unvalidated code, so a repair needs fresh validation and a new handoff",
                candidate.landing_branch, candidate.candidate.commit
            ),
        )?);
    }
    complete(host, context, observed, &evidence)
}

/// No-diff delivery: the work is already on the landing branch, so there is
/// nothing to merge. The typed evidence and completion authority are still
/// required — the owner store rechecks both — and the covering commit is
/// verified here against the real landing ref.
fn land_already_landed<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    covering_commit: &str,
    view: &LandingView,
) -> Result<Value, OrbitError> {
    let workspace = context.workspace_path.clone();
    let observed = observe_or_stop(host, context, None, view)?;
    let landing = resolve_landing_ref(
        &workspace,
        &context.candidate.landing_branch,
        &context.candidate.delivery,
    )?;
    if !contains_commit(&workspace, &landing, covering_commit)? {
        return Err(stop(
            host,
            context,
            &format!(
                "covering commit {covering_commit} is not contained in landing ref \
                 '{landing}'{}, so this delivery is not already landed",
                view.clause()
            ),
        )?);
    }
    let evidence = json!({
        "delivery": "already_landed",
        "landing_ref": landing,
        "landing_ref_refreshed": view.refreshed,
        "covering_commit": covering_commit,
        "verified": "landing_ref_contains_covering_commit",
        "external_merge": false,
    });
    complete(host, context, observed, &evidence)
}

/// A verified clean base completes without an external merge. The Core host
/// rechecks the pinned verifier report, and that the live base still contains
/// the synchronized base, at the completion write. The landing branch may
/// have advanced past that base since the run synchronized [ORB-15074];
/// `observe_or_stop` already stops when it no longer contains it.
fn land_no_diff<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    view: &LandingView,
) -> Result<Value, OrbitError> {
    let observed = observe_or_stop(host, context, None, view)?;
    let landing = resolve_landing_ref(
        &context.workspace_path,
        &observed.landing_branch,
        &observed.delivery,
    )?;
    if observed.candidate != observed.base {
        return Err(stop(
            host,
            context,
            "NoDiff candidate is not its synchronized base; revalidate the current base",
        )?);
    }
    complete(
        host,
        context,
        observed,
        &json!({
            "delivery": "no_diff", "landing_ref": landing,
            "verified": "clean_base_checkpoint", "external_merge": false,
        }),
    )
}

/// Observe the candidate, recording a durable stop when it cannot be confirmed.
///
/// A rewritten candidate, an unreachable base or a landing branch that no
/// longer resolves are all the same answer: this owner will not land what it
/// cannot verify, and the evidence says why.
fn observe_or_stop<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    status: Option<&PullRequestStatus>,
    view: &LandingView,
) -> Result<HandoffCandidate, OrbitError> {
    match observe_candidate(context, status, view) {
        Ok(observed) => Ok(observed),
        Err(error) => Err(stop(host, context, &error.to_string())?),
    }
}

/// What this activity actually read about the candidate.
///
/// The candidate and base objects are resolved in the owner checkout and their
/// trees compared with the accepted identity, so a rewritten or re-pointed
/// commit cannot pass as the validated one. For a pull request the head and
/// base branch names come from the provider's own answer; `landing_branch` is
/// confirmed to resolve as a ref here. `repository` is the provider's
/// `owner/name` when a pull request is involved, and is carried from the
/// accepted candidate for a checkout with no remote.
fn observe_candidate(
    context: &HandoffLandingContext,
    status: Option<&PullRequestStatus>,
    view: &LandingView,
) -> Result<HandoffCandidate, OrbitError> {
    let candidate = &context.candidate;
    let workspace = &context.workspace_path;
    let observed_candidate = observe_revision(workspace, &candidate.candidate, "candidate")?;
    let observed_base = observe_revision(workspace, &candidate.base, "base")?;
    let landing = resolve_landing_ref(workspace, &candidate.landing_branch, &candidate.delivery)?;
    // The validated base must still be reachable from the branch this lands on;
    // a rewritten history is not the base the evidence was produced against.
    if !matches!(candidate.delivery, HandoffDelivery::AlreadyLanded { .. })
        && !contains_commit(workspace, &landing, &observed_base.commit)?
    {
        return Err(OrbitError::Execution(format!(
            "handoff_land: validated base {} is no longer reachable from landing ref \
             '{landing}'{}",
            observed_base.commit,
            view.clause()
        )));
    }
    let (source_branch, base_branch) = match status {
        Some(status) => (
            status.head_branch.clone().ok_or_else(|| {
                OrbitError::Execution(
                    "handoff_land: pull request did not report its head branch".to_string(),
                )
            })?,
            status.base_branch.clone().ok_or_else(|| {
                OrbitError::Execution(
                    "handoff_land: pull request did not report its base branch".to_string(),
                )
            })?,
        ),
        None => (
            candidate.source_branch.clone(),
            candidate.base_branch.clone(),
        ),
    };
    Ok(HandoffCandidate {
        repository: candidate.repository.clone(),
        source_branch,
        base_branch,
        landing_branch: candidate.landing_branch.clone(),
        candidate: observed_candidate,
        base: observed_base,
        delivery: candidate.delivery.clone(),
    })
}

/// Resolve one accepted revision in the owner checkout.
///
/// The rule itself is shared with handoff acceptance [ORB-12500], so the
/// owner cannot read a candidate one way when it accepts the work and a
/// different way when it lands it.
fn observe_revision(
    workspace: &Path,
    accepted: &SourceRevision,
    label: &str,
) -> Result<SourceRevision, OrbitError> {
    super::claim::observe_accepted_revision(workspace, accepted, label, "handoff_land")
}

/// The ref a delivery actually lands on: `origin/<branch>` for published work
/// when the checkout has one, the local branch for an owner-local candidate. A
/// landing branch that resolves to neither cannot be verified.
fn resolve_landing_ref(
    workspace: &Path,
    landing_branch: &str,
    delivery: &HandoffDelivery,
) -> Result<String, OrbitError> {
    // Owner-local delivery has no remote to land on: its target is the local
    // branch, and checking `origin/` would read a ref the merge never moves.
    let refs = match delivery {
        HandoffDelivery::LocalCandidate => vec![landing_branch.to_string()],
        _ => vec![
            format!("origin/{landing_branch}"),
            landing_branch.to_string(),
        ],
    };
    for candidate in refs {
        if git_command_success(
            workspace,
            &["rev-parse", "--verify", &format!("{candidate}^{{commit}}")],
        )? {
            return Ok(candidate);
        }
    }
    Err(OrbitError::Execution(format!(
        "handoff_land: landing branch '{landing_branch}' does not resolve in the owner checkout"
    )))
}

fn contains_commit(workspace: &Path, reference: &str, commit: &str) -> Result<bool, OrbitError> {
    if !git_command_success(
        workspace,
        &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
    )? {
        return Ok(false);
    }
    git_command_success(
        workspace,
        &["merge-base", "--is-ancestor", commit, reference],
    )
}

/// [ORB-15956] Pin the pull request's reported head instead of the accepted
/// candidate when it is a verified provider branch update of that candidate.
/// Returns the adopted head. Any other head is left to the pin to refuse.
///
/// The provider merges the base as it is when the update runs, which can be
/// past the landing ref this attempt fetched, so a published landing ref is
/// fetched again before a head not yet adopted is verified against it. A
/// fetch that fails refuses the head, even if the initial refresh failed and
/// the head and base are cached [ORB-15976]. Without `origin`, verification
/// intentionally uses the owner's local landing branch.
fn adopt_base_update(
    pin: &mut DeliveryPin,
    context: &HandoffLandingContext,
    status: &PullRequestStatus,
) -> Option<String> {
    let candidate = &context.candidate;
    let head = status.head_sha.as_deref()?;
    if head == candidate.candidate.commit
        || status.head_branch.as_deref() != Some(candidate.source_branch.as_str())
    {
        return None;
    }
    if pin.candidate_sha() != Some(head)
        && git_command_success(&context.workspace_path, &["remote", "get-url", "origin"])
            .unwrap_or(false)
        && let Err(error) = fetch_remote_base(&context.workspace_path, &candidate.landing_branch)
    {
        tracing::warn!(
            head,
            %error,
            "handoff_land: the landing ref could not be refreshed to verify the pull request's \
             updated head"
        );
        return None;
    }
    let landing = resolve_landing_ref(
        &context.workspace_path,
        &candidate.landing_branch,
        &candidate.delivery,
    )
    .ok()?;
    match verified_base_update(
        &context.workspace_path,
        &candidate.candidate.commit,
        head,
        &candidate.source_branch,
        &landing,
    ) {
        Ok(()) => {
            if pin.candidate_sha() != Some(head) {
                pin.adopt_refreshed_candidate(Some(head));
            }
            Some(head.to_string())
        }
        Err(refusal) => {
            tracing::warn!(
                head,
                refusal,
                "handoff_land: the pull request's head is not a verified base update of the \
                 candidate"
            );
            None
        }
    }
}

/// The landing branch's current commit in the owner checkout, refreshed from
/// `origin` when the landing reads published delivery.
fn landing_tip(context: &HandoffLandingContext, view: &LandingView) -> Option<String> {
    let candidate = &context.candidate;
    if view.refreshed {
        let _ = fetch_remote_base(&context.workspace_path, &candidate.landing_branch);
    }
    let landing = resolve_landing_ref(
        &context.workspace_path,
        &candidate.landing_branch,
        &candidate.delivery,
    )
    .ok()?;
    git_output(
        &context.workspace_path,
        &["rev-parse", &format!("{landing}^{{commit}}")],
    )
    .ok()
}

fn pin_for(candidate: &HandoffCandidate) -> DeliveryPin {
    DeliveryPin::pinned(
        &candidate.source_branch,
        &candidate.base_branch,
        &candidate.candidate.commit,
    )
}

fn record<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    step: HandoffLandingStep,
    observed: Option<HandoffCandidate>,
    evidence: &Value,
) -> Result<(), OrbitError> {
    host.record_handoff_landing(&HandoffLandingUpdate {
        handoff_id: context.handoff_id.clone(),
        step,
        observed,
        evidence: evidence.to_string(),
    })
}

fn complete<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    observed: HandoffCandidate,
    evidence: &Value,
) -> Result<Value, OrbitError> {
    record(
        host,
        context,
        HandoffLandingStep::Complete,
        Some(observed),
        evidence,
    )?;
    Ok(json!({
        "phase": "landed",
        "handoff_id": context.handoff_id,
        "task_id": context.task_id,
        "evidence": evidence,
    }))
}

/// Record a durable stop and return the error the run fails with, so the
/// evidence an operator reads and the failure they see say the same thing.
fn stop<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    reason: &str,
) -> Result<OrbitError, OrbitError> {
    record_stop(host, context, reason, false)
}

/// [`stop`] for a candidate that conflicts with, or is stale against, the base
/// it lands on. The owner store hands it to one automatic repair.
fn stop_for_repair<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    reason: &str,
) -> Result<OrbitError, OrbitError> {
    record_stop(host, context, reason, true)
}

fn record_stop<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    reason: &str,
    repairable: bool,
) -> Result<OrbitError, OrbitError> {
    record(
        host,
        context,
        HandoffLandingStep::Stop { repairable },
        None,
        &Value::String(reason.to_string()),
    )?;
    Ok(OrbitError::Execution(format!("handoff_land: {reason}")))
}

/// Whether the provider reports the pull request's branch as behind its base,
/// which branch protection refuses until the branch is brought up to date.
fn behind_base(status: &PullRequestStatus) -> bool {
    status
        .merge_state_status
        .as_deref()
        .is_some_and(|state| state.eq_ignore_ascii_case("BEHIND"))
}

fn describe(state: &PrMergeState) -> String {
    match state {
        PrMergeState::Merged => "merged".to_string(),
        PrMergeState::Closed => "closed".to_string(),
        PrMergeState::Contradictory(reason) => format!("contradictory: {reason}"),
        PrMergeState::Blocked(reason) => format!("blocked: {reason}"),
        PrMergeState::Conflict => "conflict".to_string(),
        PrMergeState::Mergeable => "mergeable".to_string(),
        PrMergeState::Pending => "pending".to_string(),
    }
}
