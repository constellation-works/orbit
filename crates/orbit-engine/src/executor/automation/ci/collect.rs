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
//! are current when their evidence is complete; a pending check is not. A red
//! run whose workflow is still running a push on its branch at a descendant
//! commit is held in `pending_supersession` until that run completes.
//!
//! Losing the agent's ability to ask a follow-up question mid-diagnosis is the
//! accepted cost of that boundary. The compensation is that the snapshot is
//! generous and that every bound it hit is reported in `truncation` — a
//! reader must never have to guess whether "no more failures" meant "none" or
//! "we stopped looking".

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_tools::github_cli;
use serde_json::{Value, json};

use super::history::RetryableHistory;
use super::investigate::{
    inconclusive_cancellation_findings, investigate, mark_concurrency_cancellations,
};
use super::partition::{
    RunPartition, is_actionable_current_failure, is_inconclusive_cancellation, is_landing_failure,
    partition_runs, run_branch, run_is_cancelled, run_is_completed, sort_current_failures,
    supersede_older_when_cancelled_run_is_actionable, superseded_cancellation_entry,
};
use super::pending::defer_for_in_flight_descendants;
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
/// replaced. The ceiling is the largest page the run-list request will ask
/// `gh` for, so the bound reported in `truncation` is the one actually applied
/// and a full page always raises the cap note.
const DEFAULT_MAX_RUNS: u64 = 100;
const MAX_MAX_RUNS: u64 = github_cli::RUN_LIST_MAX_LIMIT;
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
/// Cap on job annotation reads that ask whether a cancelled job with a failed
/// step was cancelled by a workflow concurrency group. A job past the cap is
/// investigated as before.
const MAX_CANCELLATION_ANNOTATION_READS: usize = 12;

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
        // Both listings need at least one entry: the list requests reject a
        // zero limit, and a sweep that errors every time can never report a
        // clean result.
        max_runs: bounded_u64(input, "max_runs", DEFAULT_MAX_RUNS, MAX_MAX_RUNS)?.max(1),
        max_pull_requests: bounded_u64(
            input,
            "max_pull_requests",
            DEFAULT_MAX_PULL_REQUESTS,
            MAX_PULL_REQUESTS,
        )?
        .max(1),
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

/// Collect one CI evidence snapshot. `history` carries how many consecutive
/// collections each run-scoped retryable error has been seen in, and is
/// updated with this collection's errors.
#[cfg(test)]
pub(super) fn collect<Q: CiQueries + ?Sized>(
    queries: &Q,
    input: &Value,
    history: &mut RetryableHistory,
) -> Result<Value, OrbitError> {
    collect_for_sweep(queries, input, history, "direct-collection")
}

pub(super) fn collect_for_sweep<Q: CiQueries + ?Sized>(
    queries: &Q,
    input: &Value,
    history: &mut RetryableHistory,
    sweep_id: &str,
) -> Result<Value, OrbitError> {
    let bounds = bounds_from_input(input)?;
    let auth = queries.auth_status();
    if !auth.usable() {
        // Stop here on purpose. Every later field would be an empty list that
        // reads exactly like "nothing is failing", and that conclusion
        // requires queries this host could not run.
        history.observe(Vec::new(), sweep_id);
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
    let probes = probe_branches(queries, &branch_heads, &refs, &runs, &bounds, &mut notes);
    let mut partition = RunPartition::default();
    partition_runs(&refs, &runs, &probes, &mut partition);
    let RunPartition {
        latest,
        current,
        mut stale,
        in_flight,
        mixed_candidates,
        mut deferred,
        cancelled_successors,
        in_flight_successors,
    } = partition;
    // Before any investigation slot is spent: a red run whose identity is
    // still running on a descendant commit is not filed this sweep.
    let (mut current, pending_supersession) =
        defer_for_in_flight_descendants(queries, current, &in_flight_successors, &mut notes);

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

    // A cancelled run is screened by its view and job annotations before any
    // investigation slot or log read is spent. When every job is cancelled
    // without a failed step, or was cancelled by a workflow concurrency group
    // (whose interrupted step reads as failed), the run is superseded when a
    // newer run of its workflow/ref exists and inconclusive otherwise; either
    // way it has nothing to repair, so it must not crowd a real failure out of
    // the budget or fail the sweep over a log it never needed. A cancellation
    // with any other failed step stays a candidate and keeps its view for
    // investigation.
    let mut inspect = Vec::new();
    let mut inconclusive = Vec::new();
    let mut superseded_cancellations = 0usize;
    let mut annotation_reads = 0usize;
    let mut annotation_reads_skipped = 0usize;
    let mut screened_views = std::collections::BTreeMap::new();
    let mut seen_run_ids = std::collections::BTreeSet::new();
    for failure in current.iter().chain(mixed_candidates.iter()) {
        let Some(run_id) = failure.get("run_id").and_then(Value::as_u64) else {
            continue;
        };
        if !seen_run_ids.insert(run_id) {
            continue;
        }
        if run_is_completed(failure) && run_is_cancelled(failure) {
            // A failed screen leaves the run to ordinary investigation, which
            // queries and reports the view itself.
            if let Ok(mut view) = queries.run_view(&run_id.to_string()) {
                mark_concurrency_cancellations(
                    queries,
                    &mut view,
                    &mut annotation_reads,
                    MAX_CANCELLATION_ANNOTATION_READS,
                    &mut annotation_reads_skipped,
                );
                match inconclusive_cancellation_findings(failure, &view) {
                    Some(findings) => {
                        match cancelled_successors.get(&run_id) {
                            Some(newer) => {
                                stale.push(superseded_cancellation_entry(failure, newer));
                                superseded_cancellations += 1;
                            }
                            None => inconclusive.extend(findings),
                        }
                        continue;
                    }
                    None => {
                        screened_views.insert(run_id, view);
                    }
                }
            }
        }
        inspect.push(failure.clone());
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
        let cached_view = failure
            .get("run_id")
            .and_then(Value::as_u64)
            .and_then(|run_id| screened_views.remove(&run_id));
        findings.extend(investigate(
            queries,
            failure,
            cached_view,
            &bounds,
            &mut checkout_log_reads,
            &mut job_log_reads,
            &mut retryable_errors,
        ));
    }
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
    let observed = history.observe(retryable_errors, sweep_id);
    let retryable_errors = observed.retryable;
    let persistent_errors = observed.persistent;
    let (mut current, persistently_incomplete) =
        split_persistently_incomplete(current, &persistent_errors, &retryable_errors);
    sort_current_failures(&mut current);
    sort_current_failures(&mut inconclusive);
    let discovered = current.len();
    let (current, branch_failures): (Vec<_>, Vec<_>) = current
        .into_iter()
        .partition(|failure| is_landing_failure(&refs, failure));
    let investigated_ids = current
        .iter()
        .chain(branch_failures.iter())
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
    let pending_ids = pending_supersession
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
            "{inconclusive_count} cancelled job(s) had no failed steps or were cancelled by a \
             workflow concurrency group, and were classified inconclusive; cancellation is not \
             a pass, but there is no failed step to repair"
        ));
    }
    if annotation_reads_skipped > 0 {
        notes.push(format!(
            "{annotation_reads_skipped} cancelled job(s) with a failed step were not checked for \
             a concurrency cancellation (cap {MAX_CANCELLATION_ANNOTATION_READS}) and were \
             investigated as failures"
        ));
    }
    if !persistent_errors.is_empty() {
        notes.push(format!(
            "{} retryable error(s) repeated on {} or more consecutive sweeps for the same run, \
             job and operation; they are reported in persistent_retryable_errors and no longer \
             fail the sweep, and {} incomplete finding(s) they held are listed in \
             persistently_incomplete",
            persistent_errors.len(),
            super::history::PERSISTENT_AFTER_SWEEPS,
            persistently_incomplete.len(),
        ));
    }
    if superseded_cancellations > 0 {
        notes.push(format!(
            "{superseded_cancellations} cancelled run(s) had no failed steps and a newer run of \
             the same workflow on the same branch; they are listed in stale_or_superseded"
        ));
    }
    let investigated_count = investigated_ids.len();
    let retryable_error_count = retryable_errors.len();
    let unverified_refs = probes.unverified.keys().cloned().collect::<Vec<_>>();

    let summary = json!({
        "latest_runs_discovered": latest_ids.len(),
        "latest_run_ids": latest_ids,
        "current_failures": current_ids.len(),
        "current_failure_run_ids": current_ids,
        "branch_failures": branch_failures.len(),
        "branch_failure_run_ids": branch_failures.iter()
            .filter_map(|failure| failure.get("run_id").cloned()).collect::<Vec<_>>(),
        "investigated_failures": investigated_count,
        "investigated_failure_run_ids": investigated_ids,
        "deferred_failures": deferred_ids.len(),
        "deferred_failure_run_ids": deferred_ids,
        "pending_supersession": pending_ids.len(),
        "pending_supersession_run_ids": pending_ids,
        "inconclusive": inconclusive_count,
        "inconclusive_run_ids": inconclusive_ids,
        "superseded_cancellations": superseded_cancellations,
        "inconclusive_job_ids": inconclusive_job_ids,
        "retryable_errors": retryable_error_count,
        "persistent_retryable_errors": persistent_errors.len(),
        "persistently_incomplete_run_ids": persistently_incomplete.iter()
            .filter_map(|failure| failure.get("run_id").cloned()).collect::<Vec<_>>(),
    });
    let truncation = json!({
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
        "closed_pull_request_refs": probes.closed.keys().collect::<Vec<_>>(),
        "cancellation_annotation_reads": annotation_reads,
        "max_cancellation_annotation_reads": MAX_CANCELLATION_ANNOTATION_READS,
        "unverified_refs": unverified_refs,
        "max_retired_ref_probes": bounds.max_retired_ref_probes,
        "investigation_cursor": bounds.investigation_cursor,
        "notes": notes,
    });
    Ok(json!({
        "schema_version": CI_EVIDENCE_SCHEMA_VERSION,
        "collected": true,
        "outcome_hint": if retryable_error_count > 0 {
            OUTCOME_RETRYABLE_ERROR
        } else if current.is_empty() && branch_failures.is_empty() {
            OUTCOME_NO_CURRENT_FAILURE
        } else {
            OUTCOME_CURRENT_FAILURES
        },
        "capability": auth.to_json(),
        "repository": repository,
        "heads": refs.iter().map(head_json).collect::<Vec<_>>(),
        "latest_runs": latest,
        "current_failures": current,
        "branch_failures": branch_failures,
        "stale_or_superseded": stale,
        "in_flight": in_flight,
        "deferred": deferred,
        "pending_supersession": pending_supersession,
        "inconclusive": inconclusive,
        "retryable_errors": retryable_errors,
        "persistent_retryable_errors": persistent_errors,
        "persistently_incomplete": persistently_incomplete,
        "summary": summary,
        "truncation": truncation,
        "collected_at": chrono::Utc::now().to_rfc3339(),
    }))
}

/// Move each incomplete finding whose evidence gap has become persistent out
/// of the failure list. A finding that still has a retryable error of its own
/// stays, so filing keeps deferring it until that error degrades as well.
fn split_persistently_incomplete(
    findings: Vec<Value>,
    persistent: &[Value],
    retryable: &[Value],
) -> (Vec<Value>, Vec<Value>) {
    if persistent.is_empty() {
        return (findings, Vec::new());
    }
    let covers = |error: &Value, finding: &Value| {
        error.get("run_id").filter(|value| !value.is_null()) == finding.get("run_id")
            && error
                .get("job_id")
                .filter(|value| !value.is_null())
                .is_none_or(|job_id| finding.get("job_id") == Some(job_id))
    };
    findings.into_iter().partition(|finding| {
        finding.get("investigated").and_then(Value::as_bool) == Some(true)
            || !persistent.iter().any(|error| covers(error, finding))
            || retryable.iter().any(|error| covers(error, finding))
    })
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
