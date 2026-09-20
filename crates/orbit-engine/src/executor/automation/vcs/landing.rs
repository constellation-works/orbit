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
//!    or merges around a gate.
//!
//! The guarded `review -> done` transition is the owner store's, behind its own
//! recheck of the current authorization and the digest-pinned validation
//! evidence. This activity only supplies the merge evidence that permits it.

use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::handoff::{HandoffCandidate, HandoffDelivery};
use serde_json::{Value, json};

use crate::context::{
    HandoffLandingContext, HandoffLandingStep, HandoffLandingUpdate, RuntimeHost,
};

use super::super::ci::bounded_u64;
use super::super::input::input_string_field;
use super::git::{git_command_success, git_output, git_success};
use super::operations;
use super::pr::{DeliveryPin, PrMergeState, classify_pr_state, resolve_merge_capabilities};
use super::review_gate::revision;
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

    // An intent recorded but never resolved is this owner's own uncertainty
    // about an external write. Nothing else may happen until real state says
    // what became of it.
    if let Some(intent_id) = context.unresolved_merge_intent.clone()
        && let Some(landed) = reconcile_intent(host, &context, &intent_id)?
    {
        return Ok(landed);
    }

    match context.candidate.delivery.clone() {
        HandoffDelivery::PullRequest { number } => land_pull_request(host, &context, number, input),
        HandoffDelivery::LocalCandidate => land_local_candidate(host, &context),
        HandoffDelivery::AlreadyLanded {
            covering_commit, ..
        } => land_already_landed(host, &context, &covering_commit),
    }
}

/// Read what actually became of an unresolved merge intent and record it.
///
/// Returns the completion outcome when the external state proves the candidate
/// merged; `None` means the intent is resolved as *not* merged and this run may
/// attempt a fresh, fully rechecked landing.
fn reconcile_intent<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    intent_id: &str,
) -> Result<Option<Value>, OrbitError> {
    let candidate = &context.candidate;
    let (merged, evidence) = match &candidate.delivery {
        HandoffDelivery::PullRequest { number } => {
            let status = read_pr_status(host, &context.workspace_path, &number.to_string())?;
            match classify_pr_state(&status) {
                PrMergeState::Merged => {
                    let pin = pin_for(candidate);
                    let delivered = pin.ensure_delivered(&status, &number.to_string())?;
                    (
                        true,
                        json!({
                            "reconciled": "pull_request_merged",
                            "pull_request": number,
                            "delivery": delivered.as_json(),
                        }),
                    )
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
        HandoffDelivery::AlreadyLanded { .. } => {
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
    if !merged {
        return Ok(None);
    }
    let observed = observe_or_stop(host, context, None)?;
    Ok(Some(complete(host, context, observed, &evidence)?))
}

fn land_pull_request<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    number: u64,
    input: &Value,
) -> Result<Value, OrbitError> {
    let candidate = &context.candidate;
    let pr_number = number.to_string();
    let workspace = context.workspace_path.clone();
    let workspace_path = workspace.to_string_lossy().into_owned();
    let pin = pin_for(candidate);

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

    let mut waited_seconds = 0_u64;
    let mut intent: Option<String> = None;
    loop {
        let status = read_pr_status(host, &workspace, &pr_number)?;
        let state = classify_pr_state(&status);
        if let PrMergeState::Merged = state {
            let delivered = match pin.ensure_delivered(&status, &pr_number) {
                Ok(delivered) => delivered,
                // A merged pull request whose identity is not ours is not this
                // handoff's delivery, whatever it merged.
                Err(error) => return Err(stop(host, context, &error.to_string())?),
            };
            let evidence = json!({
                "delivery": delivered.as_json(),
                "pull_request": number,
                "candidate": candidate.candidate.commit,
            });
            let observed = observe_or_stop(host, context, Some(&status))?;
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
        match state {
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
                return Err(stop(
                    host,
                    context,
                    &format!(
                        "pull request #{pr_number} cannot be merged ({reason}); branch \
                         protection and required checks are not bypassed"
                    ),
                )?);
            }
            PrMergeState::Conflict => {
                return Err(stop(
                    host,
                    context,
                    &format!(
                        "pull request #{pr_number} conflicts with its base; the owner does not \
                         rebase unvalidated code, so a repair needs fresh validation and a new \
                         handoff"
                    ),
                )?);
            }
            PrMergeState::Mergeable if intent.is_none() => {
                let capabilities = resolve_merge_capabilities(host, &workspace_path, &pr_number)
                    .map_err(|error| {
                        OrbitError::Execution(format!(
                            "handoff_land: no permitted merge method for pull request \
                                 #{pr_number}: {error}"
                        ))
                    })?;
                let observed = observe_or_stop(host, context, Some(&status))?;
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
                        "reviewed_head_sha": candidate.candidate.commit,
                        "workspace_path": workspace_path,
                    }),
                )?;
                // Merged is what the provider reports, not what we asked for.
                continue;
            }
            // Requested already, or still waiting for required checks.
            PrMergeState::Mergeable | PrMergeState::Pending => {}
        }

        if waited_seconds >= max_wait_seconds {
            return Err(stop(
                host,
                context,
                &format!(
                    "pull request #{pr_number} did not reach a merged state within \
                     {max_wait_seconds}s; the candidate stays in review"
                ),
            )?);
        }
        let sleep_seconds = poll_interval_seconds.min(max_wait_seconds - waited_seconds);
        sleep(Duration::from_secs(sleep_seconds));
        waited_seconds = waited_seconds.saturating_add(sleep_seconds);
    }
}

/// Owner-local delivery: no origin, no pull request, and a fast-forward of the
/// landing branch is the entire external effect.
fn land_local_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
) -> Result<Value, OrbitError> {
    let candidate = &context.candidate;
    let workspace = context.workspace_path.clone();
    let observed = observe_or_stop(host, context, None)?;
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
        return Err(stop(
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
) -> Result<Value, OrbitError> {
    let workspace = context.workspace_path.clone();
    let observed = observe_or_stop(host, context, None)?;
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
                "covering commit {covering_commit} is not contained in landing ref '{landing}', \
                 so this delivery is not already landed"
            ),
        )?);
    }
    let evidence = json!({
        "delivery": "already_landed",
        "landing_ref": landing,
        "covering_commit": covering_commit,
        "verified": "landing_ref_contains_covering_commit",
        "external_merge": false,
    });
    complete(host, context, observed, &evidence)
}

/// Observe the candidate, recording a durable stop when it cannot be confirmed.
///
/// A rewritten candidate, an unreachable base or a landing branch that no
/// longer resolves are all the same answer: this owner will not land what it
/// cannot verify, and the evidence says why.
fn observe_or_stop<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    status: Option<&Value>,
) -> Result<HandoffCandidate, OrbitError> {
    match observe_candidate(context, status) {
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
    status: Option<&Value>,
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
            "handoff_land: validated base {} is no longer reachable from landing ref '{landing}'",
            observed_base.commit
        )));
    }
    let (source_branch, base_branch) = match status {
        Some(status) => (
            reported(status, "headRefName").ok_or_else(|| {
                OrbitError::Execution(
                    "handoff_land: pull request did not report its head branch".to_string(),
                )
            })?,
            reported(status, "baseRefName").ok_or_else(|| {
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

/// Resolve one accepted revision in the owner checkout, fetching it first when
/// the object is only on the remote. A tree that disagrees with the accepted
/// identity means the commit is not the one that was validated.
fn observe_revision(
    workspace: &Path,
    accepted: &SourceRevision,
    label: &str,
) -> Result<SourceRevision, OrbitError> {
    if revision(workspace, &accepted.commit).is_err() {
        let _ = git_success(workspace, &["fetch", "--quiet", "origin", &accepted.commit]);
    }
    let observed = revision(workspace, &accepted.commit).map_err(|error| {
        OrbitError::Execution(format!(
            "handoff_land: {label} commit {} is not readable in the owner checkout: {error}",
            accepted.commit
        ))
    })?;
    if observed.tree != accepted.tree {
        return Err(OrbitError::Execution(format!(
            "handoff_land: {label} commit {} has tree {} but the accepted handoff recorded {}",
            accepted.commit, observed.tree, accepted.tree
        )));
    }
    Ok(observed)
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

fn pin_for(candidate: &HandoffCandidate) -> DeliveryPin {
    DeliveryPin::pinned(
        &candidate.source_branch,
        &candidate.base_branch,
        &candidate.candidate.commit,
    )
}

fn read_pr_status<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    pr_number: &str,
) -> Result<Value, OrbitError> {
    let response = host.run_private_vcs_operation(
        operations::PR_STATUS,
        json!({
            "pr": pr_number,
            "workspace_path": workspace_path.to_string_lossy(),
        }),
    )?;
    Ok(response.get("pull_request").cloned().unwrap_or(Value::Null))
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
    record(
        host,
        context,
        HandoffLandingStep::Stop,
        None,
        &Value::String(reason.to_string()),
    )?;
    Ok(OrbitError::Execution(format!("handoff_land: {reason}")))
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

fn reported(status: &Value, field: &str) -> Option<String> {
    status
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}
