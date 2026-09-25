//! `collect_ci_evidence` — the host-owned CI discovery stage.
//!
//! Runs before any agent is launched, so the credentials it needs never have
//! to cross into a sandbox. What crosses instead is one bounded, redacted
//! snapshot: failed jobs and steps, log excerpts, the event-reported SHA and
//! the commit the runner actually checked out as separate fields, and the
//! stale/superseding evidence that says which failures are still real.
//!
//! "Still real" is decided per relevant identity: workflow, ref, and observed
//! failed jobs. Runs are listed repository-wide so a newer *relevant* success
//! can suppress an older failure, but a queued or in-progress successor is
//! not that evidence, and an unrelated pull-request run cannot erase a
//! landing-branch failure. Already-failed jobs inside an in-flight workflow
//! are current when their evidence is complete; a pending check is not.
//!
//! Losing the agent's ability to ask a follow-up question mid-diagnosis is the
//! accepted cost of that boundary. The compensation is that the snapshot is
//! generous and that every bound it hit is reported in `truncation` — a
//! reader must never have to guess whether "no more failures" meant "none" or
//! "we stopped looking".

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use serde_json::{Value, json};

use super::investigate::investigate;
use super::partition::{
    RunPartition, is_actionable_current_failure, is_inconclusive_cancellation, partition_runs,
    run_branch, run_is_completed, sort_current_failures,
    supersede_older_when_cancelled_run_is_actionable,
};
use super::query::{CiQueries, RemoteBranchHeads};
use super::refs::{RefKind, derive_refs, head_json, probe_branches};
use super::{
    OUTCOME_CAPABILITY_UNAVAILABLE, OUTCOME_CURRENT_FAILURES, OUTCOME_NO_CURRENT_FAILURE,
    OUTCOME_RETRYABLE_ERROR, bounded_u64,
};

/// Snapshot schema version. Bump when a consumer would misread an older
/// snapshot; `file_ci_failure_tasks` reads this field before anything else.
pub(super) const CI_EVIDENCE_SCHEMA_VERSION: u64 = 2;

/// Cap on the single repository-wide run listing. This is a whole-repository
/// budget, not a per-ref one: it has to be deep enough that the integration
/// head's most recent run is still in the page after the pull-request runs
/// that outnumber it, which is why it is far larger than the per-ref bound it
/// replaced.
const DEFAULT_MAX_RUNS: u64 = 100;
const MAX_MAX_RUNS: u64 = 300;
const DEFAULT_MAX_PULL_REQUESTS: u64 = 10;
const MAX_PULL_REQUESTS: u64 = 50;
const DEFAULT_MAX_INVESTIGATED_RUNS: u64 = 6;
const MAX_INVESTIGATED_RUNS: u64 = 25;
const DEFAULT_LOG_MAX_BYTES: u64 = 16_384;
const MAX_LOG_MAX_BYTES: u64 = 262_144;
/// Cap on full-log reads taken purely to evidence a checkout commit. The
/// failed-step log usually lacks it, and a full log can be tens of megabytes.
const DEFAULT_MAX_CHECKOUT_LOG_READS: u64 = 3;
/// Global cap on diagnostic reads, each bound to one failed job.
const DEFAULT_MAX_JOB_LOG_READS: u64 = 6;
const MAX_MAX_JOB_LOG_READS: u64 = 25;
/// Cap on origin probes for branches no scanned head covers. One probe per
/// distinct branch, and only for branches that actually carry a red run.
const DEFAULT_MAX_RETIRED_REF_PROBES: u64 = 20;
const MAX_RETIRED_REF_PROBES: u64 = 100;
const MAX_RETRYABLE_ERROR_CHARS: usize = 500;

pub(super) struct Bounds {
    max_runs: u64,
    pub(super) max_pull_requests: u64,
    max_investigated_runs: usize,
    pub(super) log_max_bytes: usize,
    pub(super) max_checkout_log_reads: usize,
    pub(super) max_job_log_reads: usize,
    pub(super) max_retired_ref_probes: usize,
    /// Which overflow candidate this sweep spends its rotating investigation
    /// slot on. Taken from the collection hour unless the caller pins it.
    pub(super) investigation_cursor: u64,
}

fn bounds_from_input(input: &Value) -> Result<Bounds, OrbitError> {
    Ok(Bounds {
        max_runs: bounded_u64(input, "max_runs", DEFAULT_MAX_RUNS, MAX_MAX_RUNS)?,
        max_pull_requests: bounded_u64(
            input,
            "max_pull_requests",
            DEFAULT_MAX_PULL_REQUESTS,
            MAX_PULL_REQUESTS,
        )?,
        max_investigated_runs: bounded_u64(
            input,
            "max_investigated_runs",
            DEFAULT_MAX_INVESTIGATED_RUNS,
            MAX_INVESTIGATED_RUNS,
        )? as usize,
        log_max_bytes: bounded_u64(
            input,
            "log_max_bytes",
            DEFAULT_LOG_MAX_BYTES,
            MAX_LOG_MAX_BYTES,
        )? as usize,
        max_job_log_reads: bounded_u64(
            input,
            "max_job_log_reads",
            DEFAULT_MAX_JOB_LOG_READS,
            MAX_MAX_JOB_LOG_READS,
        )? as usize,
        max_checkout_log_reads: bounded_u64(
            input,
            "max_checkout_log_reads",
            DEFAULT_MAX_CHECKOUT_LOG_READS,
            MAX_INVESTIGATED_RUNS,
        )? as usize,
        max_retired_ref_probes: bounded_u64(
            input,
            "max_retired_ref_probes",
            DEFAULT_MAX_RETIRED_REF_PROBES,
            MAX_RETIRED_REF_PROBES,
        )? as usize,
        investigation_cursor: bounded_u64(
            input,
            "investigation_cursor",
            default_investigation_cursor(),
            u64::MAX,
        )?,
    })
}

/// The sweep runs hourly, so the hour advances the rotating investigation slot
/// exactly once per sweep without any state to persist.
fn default_investigation_cursor() -> u64 {
    chrono::Utc::now().timestamp().max(0) as u64 / 3_600
}

/// Collect one CI evidence snapshot.
pub(super) fn collect<Q: CiQueries + ?Sized>(
    queries: &Q,
    input: &Value,
) -> Result<Value, OrbitError> {
    let bounds = bounds_from_input(input)?;
    let auth = queries.auth_status();
    if !auth.usable() {
        // Stop here on purpose. Every later field would be an empty list that
        // reads exactly like "nothing is failing", and that conclusion
        // requires queries this host could not run.
        return Ok(json!({
            "schema_version": CI_EVIDENCE_SCHEMA_VERSION,
            "collected": false,
            "outcome_hint": OUTCOME_CAPABILITY_UNAVAILABLE,
            "capability": auth.to_json(),
            "collected_at": chrono::Utc::now().to_rfc3339(),
        }));
    }

    let mut retryable_errors: Vec<Value> = Vec::new();
    let repository = match queries.repo_view() {
        Ok(repository) => repository,
        Err(error) => {
            push_retryable_error(
                &mut retryable_errors,
                "discovery",
                "repo_view",
                None,
                &error.to_string(),
            );
            json!({})
        }
    };
    let default_branch = repository
        .get("default_branch")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let mut notes: Vec<String> = Vec::new();
    let branch_heads = match queries.remote_branch_heads() {
        Ok(heads) => heads,
        Err(error) => RemoteBranchHeads::from_query_error(error.to_string()),
    };
    let refs = derive_refs(
        queries,
        &branch_heads,
        input,
        default_branch.as_deref(),
        &bounds,
        &mut notes,
        &mut retryable_errors,
    )?;

    // One repository-wide query rather than one per ref: a single list is what
    // lets a newer *relevant* success supersede an older failure without
    // asking the ref it ran on whether it has advanced. Selection itself is
    // scoped to workflow/ref/check identity so an unrelated pull request or
    // an in-flight successor cannot hide a landing-branch failure.
    let runs = match queries.repository_runs(bounds.max_runs) {
        Ok(runs) => runs,
        Err(error) => {
            push_retryable_error(
                &mut retryable_errors,
                "discovery",
                "run_list",
                None,
                &error.to_string(),
            );
            Vec::new()
        }
    };
    let runs_listed = runs.len();
    if runs_listed as u64 == bounds.max_runs {
        notes.push(format!(
            "repository-wide workflow runs were listed at the cap ({}); a workflow whose latest \
             run is older than the newest {} runs in this repository may be absent entirely",
            bounds.max_runs, bounds.max_runs
        ));
    }
    let probes = probe_branches(&branch_heads, &refs, &runs, &bounds, &mut notes);
    let mut partition = RunPartition::default();
    partition_runs(
        &refs,
        &runs,
        &probes.retired,
        &probes.unverified,
        &mut partition,
    );
    let RunPartition {
        latest,
        mut current,
        mut stale,
        in_flight,
        mixed_candidates,
        mut deferred,
    } = partition;

    for failure in &mut deferred {
        failure["investigated"] = json!(false);
        if let Some((operation, message)) = probes.unverified.get(run_branch(failure)) {
            push_retryable_error(
                &mut retryable_errors,
                "discovery",
                operation,
                failure.get("run_id"),
                message,
            );
        }
    }

    let mut inspect = Vec::new();
    let mut seen_run_ids = std::collections::BTreeSet::new();
    for failure in current.iter().chain(mixed_candidates.iter()) {
        let Some(run_id) = failure.get("run_id").and_then(Value::as_u64) else {
            continue;
        };
        if seen_run_ids.insert(run_id) {
            inspect.push(failure.clone());
        }
    }
    sort_current_failures(&mut inspect);
    let investigation_candidates = inspect.len();
    let selected = investigation_slots(
        investigation_candidates,
        bounds.max_investigated_runs,
        bounds.investigation_cursor,
    );
    let attempted = selected.len();
    if investigation_candidates > attempted {
        notes.push(format!(
            "{} of {investigation_candidates} current or mixed-state runs were listed but not \
             investigated (max_investigated_runs={attempted}); their run URLs are still present, \
             and the rotating slot reaches a different overflow candidate on the next sweep",
            investigation_candidates - attempted
        ));
        for (index, failure) in inspect.iter().enumerate() {
            if selected.contains(&index) || !run_is_completed(failure) {
                continue;
            }
            push_retryable_error(
                &mut retryable_errors,
                "investigation",
                "investigation_budget",
                failure.get("run_id"),
                "current failure was not investigated because max_investigated_runs was exhausted",
            );
        }
    }
    let mut checkout_log_reads = 0usize;
    let mut job_log_reads = 0usize;
    let mut findings = Vec::new();
    for (index, failure) in inspect.iter_mut().enumerate() {
        if !selected.contains(&index) {
            failure["investigated"] = json!(false);
            findings.push(failure.clone());
            continue;
        }
        findings.extend(investigate(
            queries,
            failure,
            &bounds,
            &mut checkout_log_reads,
            &mut job_log_reads,
            &mut retryable_errors,
        ));
    }
    let mut inconclusive = Vec::new();
    let mut remaining = Vec::new();
    for finding in findings {
        if is_inconclusive_cancellation(&finding) {
            inconclusive.push(finding);
            continue;
        }
        if is_actionable_current_failure(&finding) {
            remaining.push(finding);
        }
    }
    current = supersede_older_when_cancelled_run_is_actionable(remaining, &mut stale);
    sort_current_failures(&mut current);
    sort_current_failures(&mut inconclusive);
    let discovered = current.len();
    let investigated_ids = current
        .iter()
        .filter(|failure| failure.get("investigated").and_then(Value::as_bool) == Some(true))
        .filter_map(|failure| failure.get("run_id").cloned())
        .collect::<Vec<_>>();
    let latest_ids = latest
        .iter()
        .filter_map(|run| run.get("run_id").cloned())
        .collect::<Vec<_>>();
    let current_ids = current
        .iter()
        .filter_map(|run| run.get("run_id").cloned())
        .collect::<Vec<_>>();
    let deferred_ids = deferred
        .iter()
        .filter_map(|run| run.get("run_id").cloned())
        .collect::<Vec<_>>();
    let inconclusive_ids = inconclusive
        .iter()
        .filter_map(|run| run.get("run_id").cloned())
        .collect::<Vec<_>>();
    let inconclusive_job_ids = inconclusive
        .iter()
        .filter_map(|run| run.get("job_id").cloned())
        .filter(|value| !value.is_null())
        .collect::<Vec<_>>();
    let inconclusive_count = inconclusive.len();
    if inconclusive_count > 0 {
        notes.push(format!(
            "{inconclusive_count} cancelled job(s) had no failed steps and were classified \
             inconclusive; cancellation is not a pass, but there is no failed step to repair"
        ));
    }
    let investigated_count = investigated_ids.len();
    let retryable_error_count = retryable_errors.len();
    let unverified_refs = probes.unverified.keys().cloned().collect::<Vec<_>>();

    Ok(json!({
        "schema_version": CI_EVIDENCE_SCHEMA_VERSION,
        "collected": true,
        "outcome_hint": if retryable_error_count > 0 {
            OUTCOME_RETRYABLE_ERROR
        } else if current.is_empty() {
            OUTCOME_NO_CURRENT_FAILURE
        } else {
            OUTCOME_CURRENT_FAILURES
        },
        "capability": auth.to_json(),
        "repository": repository,
        "heads": refs.iter().map(head_json).collect::<Vec<_>>(),
        "latest_runs": latest,
        "current_failures": current,
        "stale_or_superseded": stale,
        "in_flight": in_flight,
        "deferred": deferred,
        "inconclusive": inconclusive,
        "retryable_errors": retryable_errors,
        "summary": {
            "latest_runs_discovered": latest_ids.len(),
            "latest_run_ids": latest_ids,
            "current_failures": current_ids.len(),
            "current_failure_run_ids": current_ids,
            "investigated_failures": investigated_count,
            "investigated_failure_run_ids": investigated_ids,
            "deferred_failures": deferred_ids.len(),
            "deferred_failure_run_ids": deferred_ids,
            "inconclusive": inconclusive_count,
            "inconclusive_run_ids": inconclusive_ids,
            "inconclusive_job_ids": inconclusive_job_ids,
            "retryable_errors": retryable_error_count,
        },
        "truncation": json!({
            "refs_scanned": refs.len(),
            "runs_listed": runs_listed,
            "max_runs": bounds.max_runs,
            "pull_requests_scanned": refs
                .iter()
                .filter(|scanned| scanned.kind == RefKind::PullRequest)
                .count(),
            "max_pull_requests": bounds.max_pull_requests,
            "current_failures_discovered": discovered,
            "current_failures_investigation_attempted": attempted,
            "current_failures_investigated": investigated_count,
            "inconclusive": inconclusive_count,
            "log_max_bytes": bounds.log_max_bytes,
            "job_log_reads": job_log_reads,
            "max_job_log_reads": bounds.max_job_log_reads,
            "checkout_log_reads": checkout_log_reads,
            "max_checkout_log_reads": bounds.max_checkout_log_reads,
            "retired_refs": probes.retired.iter().collect::<Vec<_>>(),
            "unverified_refs": unverified_refs,
            "max_retired_ref_probes": bounds.max_retired_ref_probes,
            "investigation_cursor": bounds.investigation_cursor,
            "notes": notes,
        }),
        "collected_at": chrono::Utc::now().to_rfc3339(),
    }))
}

/// Which candidates this sweep spends its investigation budget on.
///
/// The budget is smaller than the candidate list often enough that a fixed
/// prefix would mean the same runs are investigated every hour and everything
/// below the cap is never investigated at all — a permanent starvation that no
/// number of sweeps resolves. So the ranked prefix keeps all but one slot, and
/// the last slot rotates through the remainder: the landing-branch failures
/// that gate delivery still go first, and every other candidate is reached
/// within one rotation instead of never. With a budget of one there is nothing
/// to rotate and the highest-ranked candidate keeps the slot.
pub(super) fn investigation_slots(
    candidates: usize,
    budget: usize,
    cursor: u64,
) -> std::collections::BTreeSet<usize> {
    let attempted = candidates.min(budget);
    let mut slots: std::collections::BTreeSet<usize> = (0..attempted).collect();
    if candidates <= attempted || attempted < 2 {
        return slots;
    }
    let rotating = attempted - 1;
    slots.remove(&rotating);
    let overflow = candidates - rotating;
    slots.insert(rotating + (cursor % overflow as u64) as usize);
    slots
}

pub(super) fn push_retryable_error(
    errors: &mut Vec<Value>,
    stage: &str,
    operation: &str,
    run_id: Option<&Value>,
    message: &str,
) {
    let redacted = redact_all(message);
    let bounded: String = redacted.chars().take(MAX_RETRYABLE_ERROR_CHARS).collect();
    errors.push(json!({
        "stage": stage,
        "operation": operation,
        "run_id": run_id.cloned().unwrap_or(Value::Null),
        "retryable": true,
        "message": bounded,
    }));
}
