//! Completion-authorized PR delivery [ORB-11187].
//!
//! `pr_promote` ends a PR-mode run at `review`. When the operator granted this
//! invocation completion authority (`--complete`), this step carries the same
//! PR the rest of the pipeline opened the rest of the way: it drives the merge
//! through GitHub's own gates, verifies the merge actually happened, and only
//! then runs the guarded `review -> done` transition.
//!
//! The invariant this module exists to hold is that *merged* is established by
//! reading GitHub's merged state back, never inferred from having asked. In
//! particular, enabling auto-merge is not terminal success: the poll continues
//! until the PR reports `MERGED`, or the wait budget expires and the run fails
//! with the tasks still in `review`.
//!
//! Branch protection is respected by construction. The merge request is an
//! ordinary provider merge (ungated runs may use `--auto`); no administrative
//! bypass is reachable. A `BLOCKED` PR with checks still running and no
//! outstanding review is polled within the wait budget; failed checks,
//! required reviews, and unreadable check results refuse completion.
//!
//! A `DIRTY` PR is narrower than those policy refusals. With the pipeline's
//! retained `completion: done`, published-head, branch, and base checkpoints,
//! completion reuses the ordinary pinned `git_rebase` and lease-checked push
//! boundaries. Only a rebase that proves unmerged index entries can reach the
//! existing bounded conflict-recovery leaf. The retry after that recovery
//! re-pins to the base it fetches; when the base advanced past the certified
//! recovery meanwhile, `git_rebase` chases it within a bound instead of
//! refusing the recovered head [ORB-14393].
//!
//! [ORB-11982] tightens what "merged" is allowed to mean. The same checkpoints
//! also pin *which* pull request this run may deliver, so a base-modification
//! race that repoints the PR, a head this run never published, or a merged
//! state reported without a merge commit all refuse completion instead of
//! reaching the transition. See [`super::delivery`].
//!
//! [ORB-13890] A `DIRTY` PR whose head a before-PR review certified cannot be
//! merged as rebased, unreviewed content. With `re_review_on_conflict` and the
//! same checkpoints, completion rebases the branch locally through the same
//! pinned `git_rebase` (a real conflict still reaches conflict recovery),
//! publishes nothing, and returns `re_review_required` with the rebase
//! checkpoint; the pipeline then reviews, republishes and completes the new
//! head. Without the flag it stays `review_gate_stale`; the pipeline sets it
//! on every completion round but its last, so a base that keeps moving under
//! re-review ends in review rather than in an unbounded loop [ORB-14332].
//!
//! [ORB-14849] A before-landing review settles the published PR head
//! instead, after `pr_open`. Its `landing_reviewed_head_sha` then decides:
//! completion merges only that head, and when the reviewer's fix moved the
//! PR there, that head is the published one and the head it replaced the
//! previous one. The DIRTY rule above applies to it unchanged.
//!
//! [ORB-13444] Ungated completion is held to that same candidate. The
//! published head is checked before any merge or auto-merge request, and that
//! SHA is what the synchronous provider mutation requires. Pending work that
//! carries a published head waits for the mutation instead of enabling
//! auto-merge, which cannot keep the condition. A run with no published SHA
//! still uses the ungated merge.
//!
//! [ORB-14205] GitHub refuses that synchronous mutation with "Base branch was
//! modified" (HTTP 405) when another merge lands on the base between its
//! mergeability check and the request. That refusal alone is retried: after
//! the ordinary poll wait, completion re-reads the PR, re-applies every pin
//! and gate above, re-resolves the merge policy, and asks again with the same
//! SHA. At most [`BASE_MODIFIED_MERGE_ATTEMPTS`] requests are made, within the
//! unchanged wait budget.
//!
//! `auto_fix: true` (`orbit run auto --auto-fix`) authorizes repair of failed
//! required checks and merge conflicts on this pull request (see
//! [`super::auto_fix`]). Without `completion: done` beside it the step only
//! watches: it never requests a merge or completes a task, and ends once the
//! pull request is mergeable, leaving the task in review.

use super::super::remote_probe::fixed_wait as sleep;
use std::path::Path;
use std::time::Duration;

use orbit_common::{OrbitError, PrivateVcsFailure, PrivateVcsFailureClass};
use orbit_types::task::{NO_DIFF_EXPECTED_TAG, Task};
use orbit_types::workflow::{
    PR_COMPLETION_MUTATION_STARTED_EVENT, PrAutoFixBlocker, pr_completion_has_mutation_provenance,
};
use serde_json::{Value, json};

use crate::context::{ReviewLandingRequest, RuntimeHost};

use super::super::super::ci::bounded_u64;
use super::super::super::input::input_string_field;
use super::super::super::task_update::{authorization_note, complete_tasks};
use super::super::freshness::{
    branch_freshness_against_ref, commit_sha, rebase_pr_branch, remote_branch_sha,
};
use super::super::git::{base_sync_mode_from_input, resolve_worktree_start_point};
use super::super::handoff::load_handoff_context;
use super::super::operations;
use super::super::push::push_batch_changes;
use super::answers::{
    PullRequestStatus, ReviewedMergeOutcome, read_pr_status, read_pr_status_before,
};
use super::auto_fix::{
    AutoFix, BlockedVerdict, LocalRepair, admit, adopt_published_repairs, blocked_verdict,
    landed_repair, publish_local_repair, record_landed_repair, record_published_repair,
    red_base_wait, repair_failed_checks, stopped,
};
use super::delivery::{
    BlockedCheckWait, DeliveryEvidence, DeliveryPin, PrMergeState, UnreportedChecks,
    checked_pr_state, classify_pr_state,
};
use super::head_lag::HeadLag;
use super::merge::{MergeStrategy, PermittedMerge, resolve_permitted_merge};

/// Default budget for waiting out required checks before giving up.
const DEFAULT_MAX_WAIT_SECONDS: u64 = 3600;
const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 30;
const MAX_WAIT_SECONDS: u64 = 6 * 60 * 60;
const MIN_POLL_INTERVAL_SECONDS: u64 = 5;
const MAX_POLL_INTERVAL_SECONDS: u64 = 10 * 60;
/// Synchronous merge requests, the first included, that concurrent base
/// landings may refuse before completion gives up [ORB-14205].
const BASE_MODIFIED_MERGE_ATTEMPTS: u32 = 3;

pub(in crate::executor::automation) fn pr_complete<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let pinned = landing_review_pins(input);
    let input = pinned.as_ref().unwrap_or(input);
    // Resolve the bundle exactly as `pr_promote` does: the shared handoff
    // context validates that every named task still belongs to this run and has
    // not been diverted, which is the same precondition completion needs.
    let context = load_handoff_context(host, input, "pr_complete")?;

    // A `no-diff-expected` bundle delivered nothing to merge, so there is no PR
    // to verify. Its validation *is* the delivery, and completion authority
    // covers it. Untagged already-landed or verified no-diff work must instead
    // recheck the exact accepted evidence, mirroring the guard `pr_promote`
    // applies.
    let no_diff_expected = input
        .get("no_diff_expected")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let task_ids = context
        .tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    let mut delivery_fragment = None;
    let merge_outcome = if no_diff_expected {
        if let Some(checkpoint) = super::super::commit::verified_clean_tree_checkpoint(input) {
            super::super::commit::verify_clean_tree_handoff(
                host,
                &context.tasks,
                &context.workspace_path,
                input_string_field(input, "run_id")
                    .as_deref()
                    .unwrap_or(&context.batch_id),
                checkpoint,
            )?;
            // Name the covering commit in the done transition, so the landing
            // is readable from task history (and from the comment that closes
            // the task's stale PRs).
            delivery_fragment = checkpoint
                .pointer("/already_landed/covering_commit")
                .and_then(Value::as_str)
                .map(|commit| format!("already landed as {commit}"));
            json!({ "merged": false, "reason": checkpoint["decision"], "evidence": checkpoint })
        } else {
            ensure_all_tasks_no_diff_expected(&context.tasks)?;
            json!({ "merged": false, "reason": "no_diff_expected" })
        }
    } else {
        let workspace_path = context.workspace_path.to_string_lossy().into_owned();
        let pr_number = resolve_pr_number(input, &context.tasks)?;
        let auto_fix = AutoFix::from_input(host, input, &task_ids, &context.batch_id)?;
        let delivered = match drive_pr_to_merged(
            host,
            input,
            &workspace_path,
            &pr_number,
            auto_fix.as_ref(),
        )? {
            Delivery::Merged(delivered) => delivered,
            Delivery::RebasedForReview { rebased, reason } => {
                return Ok(json!({
                    "phase": "complete",
                    "no_diff_expected": false,
                    "re_review_required": true,
                    "rebased": rebased,
                    "merge": {
                        "merged": false,
                        "reason": reason,
                        "pr_number": pr_number,
                        "reviewed_head_sha": reviewed_head_sha(input),
                    },
                    "completed_task_ids": [],
                    "skipped_task_ids": [],
                }));
            }
            // Repair authority alone: the watch ends here and the task
            // stays in review.
            Delivery::Watched(outcome) => {
                return Ok(json!({
                    "phase": "complete",
                    "no_diff_expected": false,
                    "re_review_required": false,
                    "merge": outcome,
                    "completed_task_ids": [],
                    "skipped_task_ids": [],
                }));
            }
        };
        // [ORB-11333] The certificate only carries into coverage when the
        // landing that actually happened is verified against it.
        if let Some(reviewed_head_sha) = reviewed_head_sha(input) {
            host.record_review_landing(&ReviewLandingRequest {
                run_id: context.batch_id.clone(),
                task_ids: task_ids.clone(),
                workspace_path: context.workspace_path.clone(),
                pr_number: pr_number.clone(),
                base: input_string_field(input, "base").unwrap_or_default(),
                reviewed_head_sha,
                managed_merge: delivered.outcome["managed_merge"]
                    .as_bool()
                    .unwrap_or(false),
                landed_commit: delivered
                    .outcome
                    .get("landed_commit")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            })?;
        }
        delivery_fragment = Some(delivered.evidence.authorization_fragment());
        delivered.outcome
    };

    // [ORB-11982] Name the evidence that permitted `done` in durable task
    // history, so a later reader can tell this transition apart from one an
    // unattributed writer applied.
    let authorization = match delivery_fragment {
        Some(fragment) => format!(
            "{}; {fragment}",
            authorization_note(input, &context.batch_id)
        ),
        None => authorization_note(input, &context.batch_id),
    };
    let completion = complete_tasks(host, &context.batch_id, &task_ids, &authorization)?;

    Ok(json!({
        "phase": "complete",
        "no_diff_expected": no_diff_expected,
        "re_review_required": false,
        "merge": merge_outcome,
        "completed_task_ids": completion["completed_task_ids"],
        "skipped_task_ids": completion["skipped_task_ids"],
        "authorization": completion["authorization"],
    }))
}

/// The completion pins after a before-landing review settled
/// `landing_reviewed_head_sha` [ORB-14849], or `None` when no such review
/// ran. That head is the one to merge. A reviewer fix was pushed onto the
/// published head under a lease, so a different settled head is now the
/// published one and the head it replaced the previous one.
fn landing_review_pins(input: &Value) -> Option<Value> {
    let landing = input_string_field(input, "landing_reviewed_head_sha")?;
    let mut pinned = input.clone();
    if input_string_field(input, "published_head_sha").as_deref() != Some(landing.as_str()) {
        pinned["previous_published_head_sha"] = input
            .get("published_head_sha")
            .cloned()
            .unwrap_or(Value::Null);
        pinned["published_head_sha"] = json!(landing);
    }
    pinned["reviewed_head_sha"] = json!(landing);
    Some(pinned)
}

/// A merge this run is authorized to complete on, with the evidence that
/// established it.
struct DeliveredMerge {
    outcome: Value,
    evidence: DeliveryEvidence,
}

/// How driving the PR ended without an error.
enum Delivery {
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
fn drive_pr_to_merged<H: RuntimeHost + ?Sized>(
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
    let completion_started_ms = super::super::completion_deadline::now_ms()?;
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
        let elapsed_seconds = super::super::completion_deadline::now_ms()?
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
                base_candidate = super::super::base_refresh::owner_candidate(
                    Path::new(workspace_path),
                    input,
                    &pin,
                    &status,
                )?;
            }
            if let Some(candidate) = &base_candidate {
                match super::super::base_refresh::recover(
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
                    super::super::base_refresh::RefreshStep::Changed => continue,
                    super::super::base_refresh::RefreshStep::Wait => base_refresh_wait = true,
                    super::super::base_refresh::RefreshStep::Adopted { waiting } => {
                        if !base_adopted {
                            pin.adopt_refreshed_candidate(status.head_sha.as_deref());
                        }
                        base_adopted = true;
                        base_refresh_wait = waiting;
                    }
                    super::super::base_refresh::RefreshStep::None => {}
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
            match super::super::base_refresh::recover(
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
                super::super::base_refresh::RefreshStep::Changed => continue,
                super::super::base_refresh::RefreshStep::Wait
                | super::super::base_refresh::RefreshStep::Adopted { waiting: true } => {
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
                super::infrastructure::recover(
                    host,
                    auto_fix,
                    workspace_path,
                    pr_number,
                    &status,
                    diagnose,
                    completion_deadline_ms.min(
                        super::super::completion_deadline::now_ms()?.saturating_add(
                            max_wait_seconds
                                .saturating_sub(waited_seconds)
                                .saturating_mul(1000)
                        )
                    ),
                )
                .map_err(observed)?,
                super::infrastructure::InfrastructureStep::Wait
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
                        if !landed_refresh_attempted
                            && let Some(evidence) = landed_repair(
                                host,
                                input,
                                workspace_path,
                                pr_number,
                                &status,
                                &failed,
                            )
                        {
                            let from = status
                                .head_sha
                                .as_deref()
                                .ok_or_else(|| stopped(pr_number, "missing failed head"))?;
                            admit(
                                host,
                                auto_fix,
                                pr_number,
                                from,
                                PrAutoFixBlocker::FailedChecks,
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
                        }
                        if let Some(reason) =
                            red_base_wait(host, input, workspace_path, pr_number, &status, &failed)
                        {
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

fn merge_wait_timeout(
    pr_number: &str,
    waited_seconds: u64,
    max_wait_seconds: u64,
    base_race: Option<&str>,
    unreported: &UnreportedChecks,
) -> OrbitError {
    let base_race = base_race
        .map(|race| format!(" after {race}"))
        .unwrap_or_default();
    let unreported = unreported.timeout_note();
    OrbitError::Execution(format!(
        "pr_complete: timed out after {waited_seconds}s waiting for pull request \
         #{pr_number} to merge (budget {max_wait_seconds}s){base_race}{unreported}; the task \
         stays in review"
    ))
}

/// SHA the synchronous provider mutation must match.
///
/// A before-PR review wins when this run has one. Otherwise the published
/// candidate, including a head rewritten by the bounded conflict repair, is
/// the condition. With neither pin the merge stays ungated.
fn merge_condition_sha<'a>(
    reviewed_head_sha: Option<&'a str>,
    pin: &'a DeliveryPin,
) -> Option<&'a str> {
    reviewed_head_sha.or(pin.candidate_sha())
}

/// The head a before-PR gate settled, when this run carried one.
fn reviewed_head_sha(input: &Value) -> Option<String> {
    input_string_field(input, "reviewed_head_sha")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Early diagnostic for an already moved head. The provider mutation must
/// also enforce the reviewed SHA atomically; this read cannot prevent a race.
fn ensure_pr_head_is_reviewed(
    status: &PullRequestStatus,
    pr_number: &str,
    reviewed_head_sha: Option<&str>,
) -> Result<(), OrbitError> {
    let Some(reviewed) = reviewed_head_sha else {
        return Ok(());
    };
    let reported = status.head_sha.as_deref().ok_or_else(|| {
        OrbitError::Execution(format!(
            "review_gate_stale: pull request #{pr_number} did not report its head commit, so \
                 the reviewed candidate {reviewed} cannot be pinned before merging; the task stays \
                 in review"
        ))
    })?;
    if reported != reviewed {
        return Err(OrbitError::Execution(format!(
            "review_gate_stale: pull request #{pr_number} head {reported} is not the reviewed \
             candidate {reviewed}; later revisions need a fresh review before managed \
             completion, so the task stays in review"
        )));
    }
    Ok(())
}

fn re_review_on_conflict(input: &Value) -> bool {
    input
        .get("re_review_on_conflict")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Whether the PR reports `BLOCKED` itself, rather than another state
/// completion also refuses (`BEHIND`, `DRAFT`, or an explicit draft), which
/// auto-fix never repairs.
fn is_blocked_merge_state(status: &PullRequestStatus) -> bool {
    !status.is_draft
        && status
            .merge_state_status
            .as_deref()
            .is_some_and(|state| state.eq_ignore_ascii_case("BLOCKED"))
}

/// Admit an auto-fix repair of the conflict on the pull request's head
/// before the pinned rebase can reach conflict recovery.
fn admit_conflict_repair<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &AutoFix,
    pr_number: &str,
    status: &PullRequestStatus,
) -> Result<(), OrbitError> {
    let head_sha = status
        .head_sha
        .as_deref()
        .ok_or_else(|| stopped(pr_number, "the pull request reported no head commit"))?;
    admit(
        host,
        auto_fix,
        pr_number,
        head_sha,
        PrAutoFixBlocker::Conflict,
    )
    .map(|_| ())
}

/// Completion, or auto-fix repair, authority over the retained delivery
/// checkpoints a conflict repair needs.
fn has_completion_recovery_checkpoint(input: &Value) -> bool {
    (input.get("completion").and_then(Value::as_str) == Some("done")
        || input.get("auto_fix").and_then(Value::as_bool) == Some(true))
        && ["head", "published_head_sha", "base"]
            .into_iter()
            .all(|key| input_string_field(input, key).is_some())
}

fn completion_conflict_error(pr_number: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "pr_complete: pull request #{pr_number} cannot be merged (the branch has merge conflicts); \
         the task stays in review"
    ))
}

/// Reconcile a published candidate with the base that GitHub currently sees.
///
/// This deliberately composes the existing pinned rebase and lease-checked
/// push boundaries. A real conflict therefore becomes the same typed
/// `RecoverableVcsConflict` as the pre-publication synchronization step; all
/// other fetch, ownership, authorization, and push failures stay untyped and
/// cannot launch the conflict-recovery agent.
///
/// Returns the rewritten candidate SHA now published on the branch, which
/// completion adopts for subsequent provider readback and delivery checks.
fn refresh_conflicting_pr_branch<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    workspace_path: &str,
    pr_number: &str,
    pull_request: &PullRequestStatus,
) -> Result<Option<String>, OrbitError> {
    let synced =
        rebase_conflicting_pr_branch(host, input, workspace_path, pr_number, pull_request)?;
    let push_input = json!({
        "job_run_id": input.get("job_run_id").cloned().unwrap_or(Value::Null),
        "completed_task_ids": input.get("completed_task_ids").cloned().unwrap_or(Value::Null),
        "workspace_path": workspace_path,
        "branch": synced["head"],
        "rewrite_performed": synced["rewritten"],
        "rewrite_head_before": synced["head_sha_before"],
        "expected_remote_sha": synced["remote_sha_before"],
        "lease_remote_sha": synced["remote_sha_before"],
    });
    push_batch_changes(host, &push_input)?;
    Ok(synced
        .get("head_sha")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

/// Rebase the published candidate onto the base GitHub currently sees,
/// locally, returning the `git_rebase` checkpoint a lease-checked push of the
/// rewritten branch needs.
fn rebase_conflicting_pr_branch<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    workspace_path: &str,
    pr_number: &str,
    pull_request: &PullRequestStatus,
) -> Result<Value, OrbitError> {
    let head =
        input_string_field(input, "head").ok_or_else(|| completion_conflict_error(pr_number))?;
    let published_head_sha = input_string_field(input, "published_head_sha")
        .ok_or_else(|| completion_conflict_error(pr_number))?;
    let base =
        input_string_field(input, "base").ok_or_else(|| completion_conflict_error(pr_number))?;
    let workspace = std::path::Path::new(workspace_path);

    let pr_head = pull_request
        .head_branch
        .clone()
        .ok_or_else(|| completion_conflict_error(pr_number))?;
    let pr_base = pull_request
        .base_branch
        .clone()
        .ok_or_else(|| completion_conflict_error(pr_number))?;
    let expected_base = base.strip_prefix("origin/").unwrap_or(&base);
    if pr_head != head || pr_base != expected_base {
        return Err(OrbitError::Execution(format!(
            "pr_complete: pull request #{pr_number} identity changed from branch '{head}' into \
             '{expected_base}' to branch '{pr_head}' into '{pr_base}'; refusing completion \
             conflict repair"
        )));
    }

    let observed_remote_sha = remote_branch_sha(workspace, &head)?;
    if observed_remote_sha.as_deref() != Some(published_head_sha.as_str()) {
        return Err(OrbitError::Execution(format!(
            "pr_complete: published branch 'origin/{head}' moved away from completion checkpoint \
             '{published_head_sha}'; refusing to replace stale or concurrently updated candidate \
             while pull request #{pr_number} stays in review"
        )));
    }

    let base_ref =
        resolve_worktree_start_point(workspace, &base, base_sync_mode_from_input(input)?)?;
    let observed_base_sha = commit_sha(workspace, &base_ref)?;
    if let Some(pinned) = input_string_field(input, "landed_repair_base_sha")
        && pinned != observed_base_sha
    {
        return Err(stopped(
            pr_number,
            "base advanced after the landed repair was diagnosed",
        ));
    }
    let target_base_sha = observed_base_sha;
    let published_freshness =
        branch_freshness_against_ref(workspace, &published_head_sha, &base_ref, &target_base_sha)?;
    if published_freshness.commits_ahead == 0 {
        return Err(OrbitError::Execution(format!(
            "pr_complete: published candidate '{published_head_sha}' is no longer ahead of \
             target base checkpoint '{target_base_sha}'; refusing completion conflict repair"
        )));
    }
    if published_freshness.commits_behind == 0 {
        return Err(completion_conflict_error(pr_number));
    }

    let rebase_input = json!({
        "run_id": input.get("run_id").cloned().unwrap_or(Value::Null),
        "job_run_id": input.get("job_run_id").cloned().unwrap_or(Value::Null),
        "completed_task_ids": input.get("completed_task_ids").cloned().unwrap_or(Value::Null),
        "workspace_path": workspace_path,
        "head": head,
        "head_sha": published_head_sha,
        "base": base,
        "base_ref": base_ref,
        "base_sha": target_base_sha,
        "remote_sha": observed_remote_sha,
        "commits_behind": published_freshness.commits_behind,
        "sync_required": true,
    });
    record_completion_mutation(host, input, "rebase")?;
    let rebased = rebase_pr_branch(host, &rebase_input)?;
    // Auto-fix can refresh inside the watcher without returning through the
    // pipeline's validation step. Validate the exact rewrite before either
    // publication or the independent re-review handoff.
    if input.get("auto_fix").and_then(Value::as_bool) == Some(true) {
        super::super::candidate_validate::candidate_validate(
            host,
            &json!({
                "job_run_id":input["job_run_id"], "completed_task_ids":input["completed_task_ids"],
                "workspace_path":workspace_path, "base_sha":rebased["base_sha"], "base_ref":rebased["base_ref"],
            }),
        )?;
    }
    Ok(rebased)
}

/// [ORB-16307] A failed authenticated read made before any completion mutation:
/// the PR status, required-check and merge-policy reads, and the base-refresh
/// and infrastructure prerequisite reads. Only a transport failure within the
/// cumulative budget, with neither a mutation in this invocation nor durable
/// repair or mutation provenance, becomes `completion_observation_unavailable`,
/// which the host may resume only after authenticating the unchanged prefix.
/// The transport class is the one the provider boundary decided; the result
/// carries [`PrivateVcsFailureClass::CompletionObservationUnavailable`] beside
/// its unchanged message. Every other failure keeps its own error and
/// recovery path.
fn observation_failure<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    mutated: bool,
    deadline_ms: Option<u64>,
    error: OrbitError,
) -> OrbitError {
    let within_budget = deadline_ms.is_none_or(|deadline| {
        super::super::completion_deadline::now_ms().is_ok_and(|now| now < deadline)
    });
    match error.private_vcs_failure() {
        Some(failure)
            if !mutated
                && within_budget
                && failure.class == PrivateVcsFailureClass::TransientObservation
                && completion_history_is_read_only(host, input).unwrap_or(false) =>
        {
            OrbitError::PrivateVcs(Box::new(PrivateVcsFailure {
                operation: failure.operation.clone(),
                class: PrivateVcsFailureClass::CompletionObservationUnavailable,
                message: format!("completion_observation_unavailable: {error}"),
            }))
        }
        _ => error,
    }
}

/// Read reservations after pre-observation repair adoption/publication as well
/// as after polling. Absence is only eligibility: the host still authenticates
/// the exact original publication, native receipts and current policy.
fn completion_history_is_read_only<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<bool, OrbitError> {
    let context = load_handoff_context(host, input, "pr_complete")?;
    for task in context.tasks {
        if pr_completion_has_mutation_provenance(&host.get_task_history(&task.id)?) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Persist before entering a mutation boundary. In particular an unknown merge
/// reply must remain disqualified when a later invocation's local flags reset.
fn record_completion_mutation<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    operation: &str,
) -> Result<(), OrbitError> {
    let context = load_handoff_context(host, input, "pr_complete")?;
    for task in context.tasks {
        host.apply_task_automation_update(
            &task.id,
            crate::context::TaskAutomationUpdate {
                append_history: vec![orbit_types::task::TaskHistoryEntry {
                    at: chrono::Utc::now(),
                    by: "system".into(),
                    event: PR_COMPLETION_MUTATION_STARTED_EVENT.into(),
                    note: Some(
                        json!({"run_id":context.batch_id,"operation":operation,
                        "pr_number":input["pr_number"],"head_sha":input["published_head_sha"]})
                        .to_string(),
                    ),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

fn request_merge<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    workspace_path: &str,
    pr_number: &str,
    strategy: MergeStrategy,
    auto: bool,
    reviewed_head_sha: Option<&str>,
) -> Result<ReviewedMergeOutcome, OrbitError> {
    record_completion_mutation(host, input, if auto { "auto_merge" } else { "merge" })?;
    let response = host.run_private_vcs_operation(
        operations::PR_MERGE,
        json!({
            "pr": pr_number,
            "strategy": strategy.as_str(),
            "auto": auto,
            "reviewed_head_sha": reviewed_head_sha,
            "report_base_modified": reviewed_head_sha.is_some(),
            "workspace_path": workspace_path,
        }),
    )?;
    Ok(ReviewedMergeOutcome::from_response(&response))
}

fn resolved_capabilities<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &str,
    pr_number: &str,
    selected: &mut Option<PermittedMerge>,
) -> Result<PermittedMerge, OrbitError> {
    if let Some(capabilities) = selected {
        return Ok(capabilities.clone());
    }
    let capabilities =
        resolve_permitted_merge(host, workspace_path, pr_number).map_err(|error| {
            let wrapper = OrbitError::Execution(format!(
                "pr_complete: could not resolve a permitted merge method for pull request \
             #{pr_number}: {error}; the task stays in review"
            ));
            super::super::operations::carry_class(&error, wrapper)
        })?;
    *selected = Some(capabilities.clone());
    Ok(capabilities)
}

fn resolve_pr_number(input: &Value, tasks: &[Task]) -> Result<String, OrbitError> {
    if let Some(pr_number) = input_string_field(input, "pr_number") {
        return Ok(pr_number);
    }
    tasks
        .iter()
        .find_map(Task::github_pr_number)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            OrbitError::InvalidInput(
                "pr_complete: no pr_number supplied and no task in the batch carries a github-pr \
                 external ref"
                    .to_string(),
            )
        })
}

fn ensure_all_tasks_no_diff_expected(tasks: &[Task]) -> Result<(), OrbitError> {
    if tasks
        .iter()
        .any(|task| !task.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG))
    {
        return Err(OrbitError::Execution(
            "pr_complete: no_diff_expected requires every task to carry the no-diff-expected tag"
                .to_string(),
        ));
    }
    Ok(())
}
