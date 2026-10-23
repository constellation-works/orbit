//! Driving a pull request to a verified merge.

use super::super::super::remote_probe::fixed_wait as sleep;
use std::path::Path;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::workflow::PrAutoFixBlocker;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::super::ci::bounded_u64;
use super::super::super::super::input::input_string_field;
use super::super::super::push::push_batch_changes;
use super::super::answers::{ReviewedMergeOutcome, read_pr_status, read_pr_status_before};
use super::super::auto_fix::{
    AutoFix, BlockedVerdict, LocalRepair, adopt_published_repairs, base_tip_checks,
    blocked_verdict, publish_local_repair, record_base_path, record_landed_repair,
    record_published_repair, red_base_wait, repair_failed_checks, reserve_base_refresh, stopped,
};
use super::super::delivery::{
    BlockedCheckWait, DeliveryEvidence, DeliveryPin, PrMergeState, UnreportedChecks,
    checked_pr_state, classify_pr_state,
};
use super::super::head_lag::HeadLag;
use super::super::merge::PermittedMerge;
use super::conflict::{
    admit_conflict_repair, completion_conflict_error, has_completion_recovery_checkpoint,
    is_blocked_merge_state, re_review_on_conflict, rebase_conflicting_pr_branch,
    refresh_conflicting_pr_branch,
};
use super::mutation::{observation_failure, request_merge, resolved_capabilities};
use super::reviewed::{
    ensure_pr_head_is_reviewed, merge_condition_sha, merge_wait_timeout, reviewed_head_sha,
};

/// Default budget for waiting out required checks before giving up.
const DEFAULT_MAX_WAIT_SECONDS: u64 = 3600;
const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 30;
const MAX_WAIT_SECONDS: u64 = 6 * 60 * 60;
const MIN_POLL_INTERVAL_SECONDS: u64 = 5;
const MAX_POLL_INTERVAL_SECONDS: u64 = 10 * 60;
/// Synchronous merge requests, the first included, that concurrent base
/// landings may refuse before completion gives up [ORB-14205].
const BASE_MODIFIED_MERGE_ATTEMPTS: u32 = 3;

/// A merge this run is authorized to complete on, with the evidence that
/// established it.
pub(super) struct DeliveredMerge {
    pub(super) outcome: Value,
    pub(super) evidence: DeliveryEvidence,
}

/// How driving the PR ended without an error.
pub(super) enum Delivery {
    Merged(DeliveredMerge),
    /// The reviewed head conflicted with its base and was rebased locally,
    /// or an auto-fix repaired it; the checkpoint must be reviewed before it
    /// is published.
    RebasedForReview {
        rebased: Value,
        reason: &'static str,
    },
    /// An auto-fix watch without merge authority ended: the pull request is
    /// mergeable, or was merged by someone else.
    Watched(Value),
}

/// Poll GitHub until the PR is verifiably merged, requesting the merge (or
/// auto-merge) as its reported state allows.
pub(super) fn drive_pr_to_merged<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    workspace_path: &str,
    pr_number: &str,
    auto_fix: Option<&AutoFix>,
) -> Result<Delivery, OrbitError> {
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
    // One sample sets the proposed deadline and the already-waited prefix.
    // A second read can cross a millisecond boundary and charge a second no
    // poll spent (one 5s poll recorded as 6s). A persisted deadline is still
    // that host's earlier absolute instant, measured against this same sample.
    let completion_started_ms = super::super::super::completion_deadline::now_ms()?;
    let completion_deadline_ms = completion_started_ms
        .checked_add(max_wait_seconds.saturating_mul(1000))
        .ok_or_else(|| stopped(pr_number, "completion deadline overflow"))?;
    let completion_deadline_ms = match input_string_field(input, "run_id")
        .or_else(|| input_string_field(input, "job_run_id"))
    {
        Some(run_id) if max_wait_seconds > 0 => {
            host.completion_deadline(&run_id, pr_number, completion_deadline_ms)?
        }
        _ => completion_deadline_ms,
    };
    // Round the remaining budget up: only whole seconds already spent count
    // as waited, matching the truncating elapsed reads in the loop. The
    // difference uses the sample above, so a millisecond between two reads
    // cannot shrink it.
    let remaining_seconds = completion_deadline_ms
        .saturating_sub(completion_started_ms)
        .div_ceil(1000);
    let mut waited_seconds = max_wait_seconds.saturating_sub(remaining_seconds);

    let waited_before = waited_seconds;
    let mut auto_merge_requested = false;
    let mut merge_requested = false;
    let mut requested_landed_commit: Option<String> = None;
    let mut conflict_refresh_attempted = false;
    let mut landed_refresh_attempted = false;
    let mut merge_capabilities: Option<PermittedMerge> = None;
    let mut base_modified_refusals = 0_u32;
    // The latest base-modification refusal, kept for the error that ends the
    // wait.
    let mut base_race: Option<String> = None;

    let reviewed_head_sha = reviewed_head_sha(input);
    // [ORB-11982] The candidate this run is authorized to deliver. It moves
    // through authenticated repair publications, or when bounded conflict
    // repair rewrites and lease-pushes the same branch.
    let mut pin = DeliveryPin::from_input(input);
    let mut head_lag = HeadLag::new(max_wait_seconds, poll_interval_seconds);
    let mut base_candidate = None;
    let mut base_adopted = false;
    let watch_only = auto_fix.is_some_and(AutoFix::watch_only);
    // Polls of a BLOCKED head that showed no failed and no running check.
    let mut unsettled_polls = 0_u32;
    let mut unreported = UnreportedChecks::new();
    if let Some(auto_fix) = auto_fix {
        // An earlier round of this run may have published a repaired head,
        // and the executor may have committed a check repair since.
        adopt_published_repairs(host, auto_fix, pr_number, &mut pin)?;
        match publish_local_repair(
            host,
            input,
            auto_fix,
            workspace_path,
            pr_number,
            &pin,
            reviewed_head_sha.is_some(),
        )? {
            LocalRepair::None => {}
            LocalRepair::Published(head) => pin.adopt_refreshed_candidate(Some(&head)),
            LocalRepair::ForReview(rebased) => {
                return Ok(Delivery::RebasedForReview {
                    rebased,
                    reason: "auto_fix_repaired_reviewed_head",
                });
            }
        }
    }

    // [ORB-16307] The cumulative budget an observation retry must still fit.
    let observation_budget = (max_wait_seconds > 0).then_some(completion_deadline_ms);

    loop {
        // Same clock as the deadline. A fixture advances it by each logical
        // wait, so wall time between samples is not a second source.
        let elapsed_seconds = super::super::super::completion_deadline::now_ms()?
            .saturating_sub(completion_started_ms)
            / 1000;
        waited_seconds = waited_seconds.max(waited_before.saturating_add(elapsed_seconds));
        // Every flag is set immediately before a `continue` or at the end of
        // an iteration, so this covers every read made in the iteration.
        // Durable write-ahead provenance covers mutations inside it.
        let mutated = merge_requested
            || auto_merge_requested
            || conflict_refresh_attempted
            || landed_refresh_attempted;
        let observed = |error| observation_failure(host, input, mutated, observation_budget, error);
        let status = if max_wait_seconds == 0 {
            read_pr_status(host, Path::new(workspace_path), pr_number)
        } else {
            read_pr_status_before(
                host,
                Path::new(workspace_path),
                pr_number,
                completion_deadline_ms,
            )
        }
        .map_err(observed)?;
        let mut state = classify_pr_state(&status);
        let mut base_refresh_wait = false;
        if let Some(auto_fix) = auto_fix {
            pin.ensure_candidate_identity(&status, pr_number)?;
            if base_candidate.is_none() {
                base_candidate = super::super::super::base::refresh::owner_candidate(
                    Path::new(workspace_path),
                    input,
                    &pin,
                    &status,
                )?;
            }
            if let Some(candidate) = &base_candidate {
                match super::super::super::base::refresh::recover(
                    host,
                    Path::new(workspace_path),
                    &auto_fix.task_ids,
                    &auto_fix.run_id,
                    candidate,
                    &status,
                    completion_deadline_ms,
                    false,
                    None,
                )
                .map_err(observed)?
                {
                    super::super::super::base::refresh::RefreshStep::Changed => continue,
                    super::super::super::base::refresh::RefreshStep::Wait => {
                        base_refresh_wait = true
                    }
                    super::super::super::base::refresh::RefreshStep::Adopted { waiting } => {
                        if !base_adopted {
                            pin.adopt_refreshed_candidate(status.head_sha.as_deref());
                        }
                        base_adopted = true;
                        base_refresh_wait = waiting;
                    }
                    super::super::super::base::refresh::RefreshStep::None => {}
                }
            }
        }
        if matches!(
            state,
            PrMergeState::Mergeable
                | PrMergeState::Pending
                | PrMergeState::Conflict
                | PrMergeState::Blocked(_)
        ) {
            pin.ensure_candidate_identity(&status, pr_number)?;
            if let Some(delay) = head_lag.poll_delay(
                &status,
                &pin,
                reviewed_head_sha.as_deref(),
                workspace_path,
                pr_number,
                max_wait_seconds.saturating_sub(waited_seconds),
            )? {
                sleep(Duration::from_secs(delay));
                waited_seconds = waited_seconds.saturating_add(delay);
                continue;
            }
            pin.ensure_pinned_candidate(&status, pr_number)?;
            if !base_adopted {
                ensure_pr_head_is_reviewed(&status, pr_number, reviewed_head_sha.as_deref())?;
            }
        }
        state = checked_pr_state(
            host,
            Path::new(workspace_path),
            pr_number,
            &status,
            if auto_fix.is_some() {
                BlockedCheckWait::AutoFix
            } else {
                BlockedCheckWait::Owner(&[])
            },
            &mut unreported,
            completion_deadline_ms,
        )
        .map_err(|refusal| observed(refusal.into_error(|reason| stopped(pr_number, reason))))?;
        if !base_refresh_wait
            && !base_adopted
            && (matches!(state, PrMergeState::Blocked(_)) || unreported.awaiting_policy_checks())
            && let (Some(auto_fix), Some(candidate)) = (auto_fix, base_candidate.as_ref())
        {
            match super::super::super::base::refresh::recover(
                host,
                Path::new(workspace_path),
                &auto_fix.task_ids,
                &auto_fix.run_id,
                candidate,
                &status,
                completion_deadline_ms,
                true,
                None,
            )
            .map_err(observed)?
            {
                super::super::super::base::refresh::RefreshStep::Changed => continue,
                super::super::super::base::refresh::RefreshStep::Wait
                | super::super::super::base::refresh::RefreshStep::Adopted { waiting: true } => {
                    base_refresh_wait = true
                }
                _ => {}
            }
        }
        let pending_required = matches!(state, PrMergeState::Pending);
        let mut infrastructure_wait = base_refresh_wait;
        // Set only when this poll's failed checks are an inherited red base:
        // the base has not moved, or it moved and the same job has not
        // concluded. A later poll that is no longer that hold must not keep
        // the reason.
        let mut base_red_reason: Option<String> = None;
        if let Some(auto_fix) = auto_fix
            && !base_refresh_wait
            && matches!(
                state,
                PrMergeState::Mergeable | PrMergeState::Pending | PrMergeState::Blocked(_)
            )
        {
            let verdict = blocked_verdict(&status, unsettled_polls, pending_required);
            // Outstanding reviews and unreadable rollups cannot authorize a
            // rerun, including when a reservation already exists.
            let diagnose = is_blocked_merge_state(&status)
                && matches!(verdict, BlockedVerdict::RepairChecks(_));
            if let BlockedVerdict::Stop(reason) = &verdict
                && (reason.starts_with("review is required")
                    || reason == "review decision is unavailable"
                    || reason.contains("unreadable"))
            {
                return Err(stopped(pr_number, reason));
            }
            infrastructure_wait |= matches!(
                super::super::infrastructure::recover(
                    host,
                    auto_fix,
                    workspace_path,
                    pr_number,
                    &status,
                    diagnose,
                    completion_deadline_ms.min(
                        super::super::super::completion_deadline::now_ms()?.saturating_add(
                            max_wait_seconds
                                .saturating_sub(waited_seconds)
                                .saturating_mul(1000)
                        )
                    ),
                )
                .map_err(observed)?,
                super::super::infrastructure::InfrastructureStep::Wait
            );
        }
        match state {
            _ if infrastructure_wait => {}
            PrMergeState::Merged if watch_only => {
                return Ok(Delivery::Watched(json!({
                    "merged": true,
                    "reason": "auto_fix_merged_elsewhere",
                    "pr_number": pr_number,
                    "head_sha": status.head_sha,
                    "waited_seconds": waited_seconds,
                })));
            }
            PrMergeState::Merged => {
                let evidence = pin.ensure_delivered(&status, pr_number)?;
                let landed_commit = status.exact_merge_commit.as_deref();
                let managed_merge = requested_landed_commit
                    .as_deref()
                    .is_some_and(|requested| Some(requested) == landed_commit);
                return Ok(Delivery::Merged(DeliveredMerge {
                    outcome: json!({
                        "merged": true,
                        "pr_number": pr_number,
                        "strategy": merge_capabilities.as_ref().map(|capabilities| capabilities.strategy.as_str()),
                        "auto_merge_requested": auto_merge_requested,
                        "waited_seconds": waited_seconds,
                        "max_wait_seconds": max_wait_seconds,
                        "poll_interval_seconds": poll_interval_seconds,
                        "landed_commit": landed_commit,
                        "managed_merge": managed_merge,
                        "base_modified_refusals": base_modified_refusals,
                        "stale_head_observations": head_lag.observations,
                        "reviewed_head_sha": reviewed_head_sha,
                        "delivery_evidence": evidence.as_json(),
                    }),
                    evidence,
                }));
            }
            PrMergeState::Closed => {
                return Err(OrbitError::Execution(format!(
                    "pr_complete: pull request #{pr_number} was closed without being merged; \
                     the task stays in review"
                )));
            }
            PrMergeState::Contradictory(reason) => {
                return Err(OrbitError::Execution(format!(
                    "delivery_evidence_stale: pull request #{pr_number} reports a contradictory \
                     merge state ({reason}); completion needs an unambiguous merged state, so the \
                     task stays in review"
                )));
            }
            PrMergeState::Blocked(reason) => {
                let Some(auto_fix) = auto_fix.filter(|_| is_blocked_merge_state(&status)) else {
                    let diagnosis = pin
                        .base_advance_note(input, workspace_path)
                        .unwrap_or_default();
                    return Err(OrbitError::Execution(format!(
                        "pr_complete: pull request #{pr_number} cannot be merged ({reason}); \
                         branch protection or required reviews are unsatisfied and this run does \
                         not bypass them — the task stays in review.{diagnosis}"
                    )));
                };
                match blocked_verdict(&status, unsettled_polls, pending_required) {
                    BlockedVerdict::Wait => unsettled_polls += 1,
                    BlockedVerdict::Stop(reason) => return Err(stopped(pr_number, &reason)),
                    // A repair commit on a reviewed head is reviewed again
                    // before it is published; the last completion round has
                    // no re-review left to give it.
                    BlockedVerdict::RepairChecks(_)
                        if reviewed_head_sha.is_some() && !re_review_on_conflict(input) =>
                    {
                        return Err(stopped(
                            pr_number,
                            "its head is bound to a review and this completion round has no \
                             re-review left for a repair",
                        ));
                    }
                    BlockedVerdict::RepairChecks(failed) => {
                        let base_evidence = base_tip_checks(
                            host,
                            input,
                            workspace_path,
                            pr_number,
                            &status,
                            &failed,
                        );
                        if base_evidence
                            .as_ref()
                            .is_some_and(|evidence| evidence["path"] == "known_red_base")
                        {
                            let evidence = base_evidence
                                .as_ref()
                                .ok_or_else(|| stopped(pr_number, "missing base evidence"))?;
                            record_base_path(host, auto_fix, "pr_auto_fix_base_red", evidence)?;
                            base_red_reason = Some(format!(
                                "base_red: required failures also fail on current base {}, so no repair agent runs",
                                evidence["landed_base"]
                            ));
                        } else if !landed_refresh_attempted && let Some(evidence) = base_evidence {
                            let from = status
                                .head_sha
                                .as_deref()
                                .ok_or_else(|| stopped(pr_number, "missing failed head"))?;
                            let candidate = base_candidate.as_ref().ok_or_else(|| {
                                stopped(
                                    pr_number,
                                    "the base refresh lacks an owned candidate identity",
                                )
                            })?;
                            reserve_base_refresh(
                                host,
                                auto_fix,
                                candidate,
                                completion_deadline_ms,
                                &evidence,
                            )?;
                            record_landed_repair(host, auto_fix, &evidence)?;
                            let mut refresh_input = input.clone();
                            refresh_input["published_head_sha"] = json!(from);
                            // Pin the diagnosed landed base; a second base advance
                            // must be diagnosed on a later bounded attempt.
                            refresh_input["landed_repair_base_sha"] =
                                evidence["landed_base"].clone();
                            // Keep PR identity checking against the named base.
                            let rebased = rebase_conflicting_pr_branch(
                                host,
                                &refresh_input,
                                workspace_path,
                                pr_number,
                                &status,
                            )?;
                            if reviewed_head_sha.is_some() {
                                return Ok(Delivery::RebasedForReview {
                                    rebased,
                                    reason: "auto_fix_landed_check_repair",
                                });
                            }
                            push_batch_changes(
                                host,
                                &json!({
                                    "job_run_id":input["job_run_id"], "completed_task_ids":input["completed_task_ids"],
                                    "workspace_path":workspace_path, "branch":rebased["head"],
                                    "rewrite_performed":rebased["rewritten"], "rewrite_head_before":rebased["head_sha_before"],
                                    "expected_remote_sha":rebased["remote_sha_before"],
                                    "lease_remote_sha":rebased["remote_sha_before"],
                                }),
                            )?;
                            let to = rebased["head_sha"]
                                .as_str()
                                .ok_or_else(|| stopped(pr_number, "missing refreshed head"))?;
                            record_published_repair(
                                host,
                                auto_fix,
                                pr_number,
                                from,
                                to,
                                PrAutoFixBlocker::FailedChecks,
                            )?;
                            pin.adopt_refreshed_candidate(Some(to));
                            landed_refresh_attempted = true;
                            continue;
                        } else if let Some(reason) =
                            red_base_wait(host, input, workspace_path, pr_number, &status, &failed)
                        {
                            record_base_path(
                                host,
                                auto_fix,
                                "pr_auto_fix_base_red",
                                &json!({"failed_head":status.head_sha,"reason":reason}),
                            )?;
                            base_red_reason = Some(reason);
                        } else {
                            return Err(repair_failed_checks(
                                host,
                                auto_fix,
                                workspace_path,
                                pr_number,
                                &status,
                                &failed,
                            ));
                        }
                    }
                }
            }
            PrMergeState::Conflict => {
                // The pin may have adopted a contiguous publication from this
                // run or an authenticated retry ancestor. Lease that exact
                // candidate, retaining the original input as provenance. Neither
                // local HEAD nor a provider observation grants this authority.
                let mut refresh_input = input.clone();
                if let Some(candidate) = pin.candidate_sha() {
                    refresh_input["published_head_sha"] = json!(candidate);
                }
                // [ORB-11333] Repairing a conflict rewrites the candidate; a
                // reviewed head cannot be merged as unreviewed content.
                // [ORB-13890] The pipeline may ask for the rebase so it can
                // review the new head before republishing it.
                if reviewed_head_sha.is_some() {
                    if !re_review_on_conflict(input) || !has_completion_recovery_checkpoint(input) {
                        return Err(OrbitError::Execution(format!(
                            "review_gate_stale: pull request #{pr_number} has merge conflicts and \
                             its head is bound to a before-PR review; a conflict repair needs a \
                             fresh review before managed completion, so the task stays in review"
                        )));
                    }
                    if let Some(auto_fix) = auto_fix {
                        admit_conflict_repair(host, auto_fix, pr_number, &status)?;
                    }
                    let rebased = rebase_conflicting_pr_branch(
                        host,
                        &refresh_input,
                        workspace_path,
                        pr_number,
                        &status,
                    )?;
                    return Ok(Delivery::RebasedForReview {
                        rebased,
                        reason: "reviewed_head_conflicted",
                    });
                }
                if conflict_refresh_attempted || !has_completion_recovery_checkpoint(input) {
                    return Err(completion_conflict_error(pr_number));
                }
                if let Some(auto_fix) = auto_fix {
                    admit_conflict_repair(host, auto_fix, pr_number, &status)?;
                }
                let refreshed = refresh_conflicting_pr_branch(
                    host,
                    &refresh_input,
                    workspace_path,
                    pr_number,
                    &status,
                )?;
                if let (Some(auto_fix), Some(from), Some(to)) =
                    (auto_fix, pin.candidate_sha(), refreshed.as_deref())
                {
                    record_published_repair(
                        host,
                        auto_fix,
                        pr_number,
                        from,
                        to,
                        PrAutoFixBlocker::Conflict,
                    )?;
                }
                pin.adopt_refreshed_candidate(refreshed.as_deref());
                conflict_refresh_attempted = true;
                // The existing PR remains authoritative. Re-read its state
                // after the branch update rather than inferring mergeability
                // from a successful push.
                continue;
            }
            PrMergeState::Mergeable if watch_only => {
                return Ok(Delivery::Watched(json!({
                    "merged": false,
                    "reason": "auto_fix_mergeable",
                    "pr_number": pr_number,
                    "head_sha": status.head_sha,
                    "waited_seconds": waited_seconds,
                    "stale_head_observations": head_lag.observations,
                })));
            }
            PrMergeState::Mergeable => {
                if !merge_requested {
                    let capabilities = resolved_capabilities(
                        host,
                        workspace_path,
                        pr_number,
                        &mut merge_capabilities,
                    )
                    .map_err(observed)?;
                    let condition = merge_condition_sha(
                        if base_adopted {
                            None
                        } else {
                            reviewed_head_sha.as_deref()
                        },
                        &pin,
                    );
                    match request_merge(
                        host,
                        input,
                        workspace_path,
                        pr_number,
                        capabilities.strategy,
                        false,
                        condition,
                    )
                    .map_err(|error| {
                        OrbitError::Execution(format!(
                            "pr_complete: could not request {} merge on pull request \
                                 #{pr_number}: {error}; the task stays in review",
                            capabilities.strategy.as_str()
                        ))
                    })? {
                        ReviewedMergeOutcome::Accepted { landed_commit } => {
                            requested_landed_commit = landed_commit;
                            merge_requested = true;
                            // Re-read rather than assuming the request landed.
                            continue;
                        }
                        ReviewedMergeOutcome::BaseModified { diagnostic } => {
                            base_modified_refusals += 1;
                            let race = format!(
                                "synchronous merge attempt {base_modified_refusals} of \
                                 {BASE_MODIFIED_MERGE_ATTEMPTS} conditioned on {} was refused \
                                 because the base branch was modified ({diagnostic})",
                                condition.unwrap_or_default()
                            );
                            if base_modified_refusals >= BASE_MODIFIED_MERGE_ATTEMPTS {
                                return Err(OrbitError::Execution(format!(
                                    "pr_complete: on pull request #{pr_number}, {race}; that is \
                                     the attempt ceiling, so the task stays in review"
                                )));
                            }
                            tracing::warn!(
                                pr_number,
                                attempt = base_modified_refusals,
                                "pr_complete: merge refused by a concurrent base change; \
                                 re-reading the pull request before asking again"
                            );
                            base_race = Some(race);
                            // The base moved, so its merge policy may have too.
                            merge_capabilities = None;
                            // Wait one poll, then re-read: nothing here proves
                            // the next request is permitted.
                        }
                    }
                }
            }
            PrMergeState::Pending => {
                // Auto-merge cannot retain a head condition. A reviewed head
                // or a published candidate waits for the synchronous mutation
                // once checks settle, including on queue-only branches where
                // that mutation refuses the unsupported merge. Only an
                // unpinned ungated run may hand the merge to GitHub.
                if !watch_only
                    && reviewed_head_sha.is_none()
                    && pin.candidate_sha().is_none()
                    && !auto_merge_requested
                {
                    // Required checks are still running. Hand the merge to
                    // GitHub's auto-merge when this repository allows it, then
                    // keep polling: enabling it is not success. Repositories
                    // with auto-merge disabled instead wait for a normally
                    // mergeable state, at which point the ordinary merge path
                    // above makes the same permitted request.
                    let capabilities = resolved_capabilities(
                        host,
                        workspace_path,
                        pr_number,
                        &mut merge_capabilities,
                    )
                    .map_err(observed)?;
                    if capabilities.auto_merge_allowed {
                        request_merge(
                            host,
                            input,
                            workspace_path,
                            pr_number,
                            capabilities.strategy,
                            true,
                            None,
                        )
                        .map_err(|error| {
                            OrbitError::Execution(format!(
                                "pr_complete: could not enable auto-merge using {} on pull request \
                                     #{pr_number}: {error}; the task stays in review",
                                capabilities.strategy.as_str()
                            ))
                        })?;
                        auto_merge_requested = true;
                    }
                }
            }
        }

        if waited_seconds >= max_wait_seconds {
            if let Some(reason) = base_red_reason.as_deref() {
                return Err(stopped(pr_number, reason));
            }
            return Err(merge_wait_timeout(
                pr_number,
                waited_seconds,
                max_wait_seconds,
                base_race.as_deref(),
                &unreported,
            ));
        }
        let remaining_seconds = max_wait_seconds.saturating_sub(waited_seconds);
        let sleep_seconds = poll_interval_seconds.min(remaining_seconds);
        sleep(Duration::from_secs(sleep_seconds));
        waited_seconds = waited_seconds.saturating_add(sleep_seconds);

        if waited_seconds >= max_wait_seconds {
            if let Some(reason) = base_red_reason.as_deref() {
                return Err(stopped(pr_number, reason));
            }
            return Err(merge_wait_timeout(
                pr_number,
                waited_seconds,
                max_wait_seconds,
                base_race.as_deref(),
                &unreported,
            ));
        }
    }
}
