//! Classification of repository-wide runs into current, stale, in-flight and
//! deferred evidence.

use serde_json::{Value, json};

use super::refs::{RefKind, ScannedRef};
use super::unsuccessful_conclusion;

/// Where each run lands once it has been classified.
#[derive(Default)]
pub(super) struct RunPartition {
    pub(super) latest: Vec<Value>,
    pub(super) current: Vec<Value>,
    pub(super) stale: Vec<Value>,
    pub(super) in_flight: Vec<Value>,
    pub(super) mixed_candidates: Vec<Value>,
    pub(super) deferred: Vec<Value>,
}

/// Classify repository-wide runs by relevant workflow/ref identity.
///
/// `latest_runs` still records the newest run of each workflow for audit.
/// Current failures are the newest unsuccessful completed run per
/// `(workflow, head_branch)` that a *relevant* newer success has not
/// suppressed. A queued or in-progress successor is listed in `in_flight`
/// and, when it is the newest run of that identity, also becomes a mixed-state
/// candidate so already-failed jobs can be observed. Landing-branch
/// (integration/release) failures are only suppressed by a newer completed
/// non-unsuccessful run of the same workflow on the same ref. Non-landing
/// failures may also be suppressed by a landing-branch success of that
/// workflow, so an abandoned Dependabot run does not revive after the
/// integration head has gone green.
pub(super) fn partition_runs(
    refs: &[ScannedRef],
    runs: &[Value],
    retired: &std::collections::BTreeSet<String>,
    unverified: &std::collections::BTreeMap<String, (String, String)>,
    out: &mut RunPartition,
) {
    let landing_branches = landing_branch_names(refs);
    let mut workflows = std::collections::BTreeMap::<String, Vec<&Value>>::new();
    for run in runs {
        let workflow = run
            .get("workflow")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        workflows.entry(workflow).or_default().push(run);
    }

    for workflow_runs in workflows.values_mut() {
        workflow_runs.sort_by_key(|run| std::cmp::Reverse(run_order(run)));
        let Some(latest) = workflow_runs.first().copied() else {
            continue;
        };
        out.latest
            .push(run_summary(ref_for_run(refs, latest), latest));

        let landing_success = workflow_runs.iter().copied().find(|run| {
            run_is_completed(run)
                && !run_is_unsuccessful(run)
                && landing_branches.contains(run_branch(run))
        });

        let mut by_ref = std::collections::BTreeMap::<String, Vec<&Value>>::new();
        for run in workflow_runs.iter().copied() {
            by_ref
                .entry(run_branch(run).to_string())
                .or_default()
                .push(run);
        }

        for ref_runs in by_ref.values_mut() {
            ref_runs.sort_by_key(|run| std::cmp::Reverse(run_order(run)));
            let same_ref_success = ref_runs
                .iter()
                .copied()
                .find(|run| run_is_completed(run) && !run_is_unsuccessful(run));
            let landing = ref_runs
                .first()
                .copied()
                .is_some_and(|run| landing_branches.contains(run_branch(run)));
            let suppressor = same_ref_success.or(if landing { None } else { landing_success });

            let mut seen_current = false;
            let mut seen_in_flight = false;
            for run in ref_runs.iter().copied() {
                if !run_is_completed(run) {
                    out.in_flight.push(run_summary(ref_for_run(refs, run), run));
                    if !seen_in_flight
                        && !unverified.contains_key(run_branch(run))
                        && suppressor.is_none_or(|success| run_order(run) > run_order(success))
                    {
                        out.mixed_candidates
                            .push(run_summary(ref_for_run(refs, run), run));
                    }
                    seen_in_flight = true;
                    continue;
                }
                if !run_is_unsuccessful(run) {
                    continue;
                }
                if retired.contains(run_branch(run)) {
                    out.stale.push(retired_ref_entry(refs, run));
                    continue;
                }
                if let Some(success) =
                    suppressor.filter(|success| run_order(success) > run_order(run))
                {
                    out.stale.push(stale_entry(
                        refs,
                        run,
                        success,
                        "superseded_by_newer_workflow_run",
                    ));
                    continue;
                }
                if seen_current {
                    if let Some(newer) = ref_runs.iter().copied().find(|candidate| {
                        run_is_completed(candidate)
                            && run_is_unsuccessful(candidate)
                            && run_order(candidate) > run_order(run)
                    }) {
                        out.stale.push(stale_entry(
                            refs,
                            run,
                            newer,
                            "superseded_by_newer_workflow_run",
                        ));
                    }
                    continue;
                }
                if unverified.contains_key(run_branch(run)) {
                    out.deferred.push(run_summary(ref_for_run(refs, run), run));
                    seen_current = true;
                    continue;
                }
                out.current.push(run_summary(ref_for_run(refs, run), run));
                // A cancelled run is still inspected, but job expansion has to
                // decide whether it is actionable. Claiming the current slot
                // here would hide an older real failure behind a zero-step
                // cancellation that has nothing to repair.
                if !run_is_cancelled(run) {
                    seen_current = true;
                }
            }
        }
    }
}

fn landing_branch_names(refs: &[ScannedRef]) -> std::collections::BTreeSet<&str> {
    refs.iter()
        .filter(|scanned| matches!(scanned.kind, RefKind::Integration | RefKind::Release))
        .map(|scanned| scanned.branch.as_str())
        .collect()
}

pub(super) fn run_branch(run: &Value) -> &str {
    run.get("head_branch").and_then(Value::as_str).unwrap_or("")
}

pub(super) fn run_is_completed(run: &Value) -> bool {
    run.get("status").and_then(Value::as_str) == Some("completed")
}

pub(super) fn run_is_unsuccessful(run: &Value) -> bool {
    unsuccessful_conclusion(run.get("conclusion").and_then(Value::as_str))
}

fn run_is_cancelled(run: &Value) -> bool {
    run.get("conclusion").and_then(Value::as_str) == Some("cancelled")
}

/// A cancelled job with no failed step is not a repair target: GitHub often
/// reports `steps: []` and 404s the job log. Cancellation is still not a pass.
pub(super) fn job_is_cancelled_without_failed_steps(job: &Value) -> bool {
    job.get("conclusion").and_then(Value::as_str) == Some("cancelled")
        && job
            .get("failed_steps")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
}

pub(super) fn is_inconclusive_cancellation(failure: &Value) -> bool {
    if failure.get("evidence_state").and_then(Value::as_str) == Some("inconclusive") {
        return true;
    }
    match failure.get("failed_jobs").and_then(Value::as_array) {
        Some(jobs) if !jobs.is_empty() => jobs.iter().all(job_is_cancelled_without_failed_steps),
        Some(_) | None => {
            // An unexpanded cancelled run might still hide failed steps.
            run_is_cancelled(failure)
                && failure.get("investigated").and_then(Value::as_bool) == Some(true)
        }
    }
}

fn has_failed_jobs(failure: &Value) -> bool {
    failure
        .get("failed_jobs")
        .and_then(Value::as_array)
        .is_some_and(|jobs| !jobs.is_empty())
}

pub(super) fn is_actionable_current_failure(failure: &Value) -> bool {
    if run_is_completed(failure) {
        return run_is_unsuccessful(failure);
    }
    has_failed_jobs(failure)
}

/// A red run on a branch origin no longer has. Not superseded by a newer run —
/// there is simply no ref left for the failure to be current on.
fn retired_ref_entry(refs: &[ScannedRef], run: &Value) -> Value {
    let mut entry = run_summary(ref_for_run(refs, run), run);
    entry["reason"] = json!("ref_no_longer_exists");
    entry["evidence"] = json!(format!(
        "branch '{}' has no head on origin: its pull request was merged or the branch was \
         deleted, so this run describes code that is either already landed — where the landing \
         branch's own runs are the current evidence — or abandoned",
        run_branch(run)
    ));
    entry
}

/// After job expansion, a cancelled run that *does* have failed steps is the
/// current evidence for that workflow/ref. Older unresolved findings of the
/// same identity then become stale — the same rule partition already applies
/// to ordinary failures, which a zero-step cancellation is not allowed to
/// trigger on its own.
pub(super) fn supersede_older_when_cancelled_run_is_actionable(
    findings: Vec<Value>,
    stale: &mut Vec<Value>,
) -> Vec<Value> {
    let mut newest_cancelled: std::collections::BTreeMap<(String, String), (String, u64, Value)> =
        std::collections::BTreeMap::new();
    for finding in &findings {
        if !run_is_cancelled(finding) {
            continue;
        }
        let key = workflow_ref_key(finding);
        let order = run_order(finding);
        let replace = newest_cancelled
            .get(&key)
            .is_none_or(|(existing_time, existing_id, _)| {
                order > (existing_time.clone(), *existing_id)
            });
        if replace {
            newest_cancelled.insert(key, (order.0, order.1, finding.clone()));
        }
    }

    let mut kept = Vec::new();
    let mut superseded_runs = std::collections::BTreeSet::new();
    for finding in findings {
        let key = workflow_ref_key(&finding);
        let Some((_, _, newer)) = newest_cancelled.get(&key) else {
            kept.push(finding);
            continue;
        };
        if run_order(&finding) < run_order(newer) {
            let run_id = finding.get("run_id").and_then(Value::as_u64).unwrap_or(0);
            if superseded_runs.insert((key.0.clone(), key.1.clone(), run_id)) {
                stale.push(stale_from_findings(&finding, newer));
            }
            continue;
        }
        kept.push(finding);
    }
    kept
}

fn workflow_ref_key(failure: &Value) -> (String, String) {
    (
        failure
            .get("workflow")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        run_branch(failure).to_string(),
    )
}

fn stale_from_findings(older: &Value, newer: &Value) -> Value {
    let mut entry = older.clone();
    entry["reason"] = json!("superseded_by_newer_workflow_run");
    entry["evidence"] = json!(format!(
        "newer relevant run {} at {} is {} with conclusion {}",
        newer
            .get("run_id")
            .and_then(Value::as_u64)
            .map_or_else(|| "unknown".to_string(), |id| id.to_string()),
        newer
            .get("created_at")
            .and_then(Value::as_str)
            .unwrap_or("an unknown time"),
        newer
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("in an unknown state"),
        newer
            .get("conclusion")
            .and_then(Value::as_str)
            .unwrap_or("not yet completed"),
    ));
    entry["superseded_by"] = json!({
        "run_id": newer.get("run_id"),
        "url": newer.get("url"),
        "created_at": newer.get("created_at"),
        "status": newer.get("status"),
        "conclusion": newer.get("conclusion"),
    });
    entry
}

fn stale_entry(refs: &[ScannedRef], older: &Value, newer: &Value, reason: &str) -> Value {
    let mut entry = run_summary(ref_for_run(refs, older), older);
    entry["reason"] = json!(reason);
    entry["evidence"] = json!(format!(
        "newer relevant run {} at {} is {} with conclusion {}",
        newer
            .get("run_id")
            .and_then(Value::as_u64)
            .map_or_else(|| "unknown".to_string(), |id| id.to_string()),
        newer
            .get("created_at")
            .and_then(Value::as_str)
            .unwrap_or("an unknown time"),
        newer
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("in an unknown state"),
        newer
            .get("conclusion")
            .and_then(Value::as_str)
            .unwrap_or("not yet completed"),
    ));
    entry["superseded_by"] = json!({
        "run_id": newer.get("run_id"),
        "url": newer.get("url"),
        "created_at": newer.get("created_at"),
        "status": newer.get("status"),
        "conclusion": newer.get("conclusion"),
    });
    entry
}

/// Sortable position of a run: creation time first, run id as the tiebreak.
fn run_order(run: &Value) -> (String, u64) {
    (
        run.get("created_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        run.get("run_id").and_then(Value::as_u64).unwrap_or(0),
    )
}

fn ref_for_run<'a>(refs: &'a [ScannedRef], run: &Value) -> Option<&'a ScannedRef> {
    let branch = run.get("head_branch").and_then(Value::as_str)?;
    refs.iter().find(|scanned| scanned.branch == branch)
}

fn run_summary(scanned: Option<&ScannedRef>, run: &Value) -> Value {
    json!({
        "run_id": run.get("run_id"),
        "workflow": run.get("workflow"),
        "title": run.get("title"),
        "status": run.get("status"),
        "conclusion": run.get("conclusion"),
        "event": run.get("event"),
        "url": run.get("url"),
        "created_at": run.get("created_at"),
        "head_branch": run.get("head_branch"),
        "ref_kind": scanned.map(|scanned| scanned.kind.as_str()).unwrap_or("other"),
        "pr_number": scanned.and_then(|scanned| scanned.pr_number.clone()),
        "pr_url": scanned.and_then(|scanned| scanned.pr_url.clone()),
        // Three commits that are routinely conflated and are kept apart here:
        // what the event reported, what the ref points at now, and — filled in
        // by `investigate` — what the runner actually checked out.
        "event_reported_head_sha": run.get("reported_head_sha"),
        "current_ref_head_sha": scanned.and_then(|scanned| scanned.head_sha.clone()),
        "actual_checkout_shas": Value::Array(Vec::new()),
        "investigated": false,
    })
}

/// Integration first, then release, then pull requests, then other refs;
/// newest run first within each. Investigation budget therefore lands on the
/// heads that gate delivery before pull requests, and on verified pull
/// requests before any other branch.
pub(super) fn sort_current_failures(failures: &mut [Value]) {
    let rank = |value: &Value| match value.get("ref_kind").and_then(Value::as_str) {
        Some("integration") => 0,
        Some("release") => 1,
        Some("pull_request") => 2,
        _ => 3,
    };
    failures.sort_by(|left, right| {
        rank(left)
            .cmp(&rank(right))
            .then_with(|| run_order(right).cmp(&run_order(left)))
    });
}
