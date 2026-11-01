//! Driving a pull request to a verified merge.

use orbit_common::OrbitError;
use serde_json::Value;

use crate::context::RuntimeHost;

use super::super::super::super::ci::bounded_u64;
use super::super::super::super::input::input_string_field;
use super::super::auto_fix::{
    AutoFix, LocalRepair, adopt_published_repairs, publish_local_repair, stopped,
};
use super::super::delivery::{DeliveryEvidence, DeliveryPin, UnreportedChecks};
use super::super::head_lag::HeadLag;
use super::super::merge::PermittedMerge;
use super::ci_rerun_hold::CiRerunHold;
use super::drive_phases::PollDriver;
use super::reviewed::{reviewed_head_sha, selective_selection_head_sha};

/// Default budget for waiting out required checks before giving up.
const DEFAULT_MAX_WAIT_SECONDS: u64 = 3600;
const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 30;
const MAX_WAIT_SECONDS: u64 = 6 * 60 * 60;
const MIN_POLL_INTERVAL_SECONDS: u64 = 5;
const MAX_POLL_INTERVAL_SECONDS: u64 = 10 * 60;

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
    let mut diagnostic = None;
    let result = drive_pr_to_merged_observed(
        host,
        input,
        workspace_path,
        pr_number,
        auto_fix,
        &mut diagnostic,
    );
    let result = result.map_err(|error| {
        super::super::super::landing::checks::rerun_diagnostic::append_ci_rerun_diagnostic(
            error,
            diagnostic.as_ref(),
        )
    });
    if let Some(auto_fix) = auto_fix {
        super::super::super::landing::checks::rerun_diagnostic::record_ci_rerun_outcome(
            host,
            &auto_fix.task_ids,
            &auto_fix.run_id,
            &result,
        );
    }
    result
}

fn drive_pr_to_merged_observed<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    workspace_path: &str,
    pr_number: &str,
    auto_fix: Option<&AutoFix>,
    diagnostic: &mut Option<
        super::super::super::landing::checks::rerun_diagnostic::CiRerunDiagnostic,
    >,
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
    let waited_seconds = max_wait_seconds.saturating_sub(remaining_seconds);

    let waited_before = waited_seconds;
    let auto_merge_requested = false;
    let merge_requested = false;
    let requested_landed_commit: Option<String> = None;
    let conflict_refresh_attempted = false;
    let landed_refresh_attempted = false;
    let merge_capabilities: Option<PermittedMerge> = None;
    let base_modified_refusals = 0_u32;
    // The latest base-modification refusal, kept for the error that ends the
    // wait.
    let base_race: Option<String> = None;

    let reviewed_head_sha = reviewed_head_sha(input);
    let selective_head_sha = selective_selection_head_sha(input);
    // [ORB-11982] The candidate this run is authorized to deliver. It moves
    // through authenticated repair publications, or when bounded conflict
    // repair rewrites and lease-pushes the same branch.
    let mut pin = DeliveryPin::from_input(input);
    let head_lag = HeadLag::new(max_wait_seconds, poll_interval_seconds);
    let base_candidate = None;
    let base_adopted = false;
    let watch_only = auto_fix.is_some_and(AutoFix::watch_only);
    // Polls of a BLOCKED head that showed no failed and no running check.
    let unsettled_polls = 0_u32;
    let unreported = UnreportedChecks::new();
    let ci_rerun_hold = CiRerunHold::default();
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

    PollDriver {
        host,
        input,
        workspace_path,
        pr_number,
        auto_fix,
        diagnostic,
        max_wait_seconds,
        poll_interval_seconds,
        completion_started_ms,
        completion_deadline_ms,
        waited_seconds,
        waited_before,
        observation_budget,
        pin,
        head_lag,
        base_candidate,
        base_adopted,
        reviewed_head_sha,
        selective_head_sha,
        watch_only,
        unsettled_polls,
        unreported,
        ci_rerun_hold,
        auto_merge_requested,
        merge_requested,
        requested_landed_commit,
        conflict_refresh_attempted,
        landed_refresh_attempted,
        merge_capabilities,
        base_modified_refusals,
        base_race,
        mutated: false,
    }
    .run()
}
