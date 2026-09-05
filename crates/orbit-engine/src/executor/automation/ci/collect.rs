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

use super::query::{CiQueries, LogScope};
use super::{
    OUTCOME_CAPABILITY_UNAVAILABLE, OUTCOME_CURRENT_FAILURES, OUTCOME_NO_CURRENT_FAILURE,
    OUTCOME_RETRYABLE_ERROR, bounded_u64, optional_input_string, unsuccessful_conclusion,
};

/// Snapshot schema version. Bump when a consumer would misread an older
/// snapshot; `file_ci_failure_tasks` reads this field before anything else.
pub(super) const CI_EVIDENCE_SCHEMA_VERSION: u64 = 1;

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
/// Cap on origin probes for branches no scanned head covers. One probe per
/// distinct branch, and only for branches that actually carry a red run.
const DEFAULT_MAX_RETIRED_REF_PROBES: u64 = 20;
const MAX_RETIRED_REF_PROBES: u64 = 100;
const MAX_RETRYABLE_ERROR_CHARS: usize = 500;

/// Which of the workspace's heads a run belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefKind {
    Integration,
    Release,
    PullRequest,
}

impl RefKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Integration => "integration",
            Self::Release => "release",
            Self::PullRequest => "pull_request",
        }
    }
}

/// One head to scan, with the SHA it currently points at.
struct ScannedRef {
    kind: RefKind,
    branch: String,
    head_sha: Option<String>,
    pr_number: Option<Value>,
    pr_url: Option<Value>,
}

struct Bounds {
    max_runs: u64,
    max_pull_requests: u64,
    max_investigated_runs: usize,
    log_max_bytes: usize,
    max_checkout_log_reads: usize,
    max_retired_ref_probes: usize,
    /// Which overflow candidate this sweep spends its rotating investigation
    /// slot on. Taken from the collection hour unless the caller pins it.
    investigation_cursor: u64,
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
    let refs = derive_refs(
        queries,
        input,
        default_branch.as_deref(),
        &bounds,
        &mut notes,
        &mut retryable_errors,
    )?;

    let mut partition = RunPartition::default();
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
    let retired = retired_branches(queries, &refs, &runs, &bounds, &mut notes);
    partition_runs(&refs, &runs, &retired, &mut partition);
    let RunPartition {
        latest,
        mut current,
        stale,
        in_flight,
        mixed_candidates,
    } = partition;

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
    for (index, failure) in inspect.iter_mut().enumerate() {
        if !selected.contains(&index) {
            failure["investigated"] = json!(false);
            continue;
        }
        investigate(
            queries,
            failure,
            &bounds,
            &mut checkout_log_reads,
            &mut retryable_errors,
        );
    }
    current = inspect
        .into_iter()
        .filter(is_actionable_current_failure)
        .collect();
    sort_current_failures(&mut current);
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
    let investigated_count = investigated_ids.len();
    let retryable_error_count = retryable_errors.len();

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
        "retryable_errors": retryable_errors,
        "summary": {
            "latest_runs_discovered": latest_ids.len(),
            "latest_run_ids": latest_ids,
            "current_failures": current_ids.len(),
            "current_failure_run_ids": current_ids,
            "investigated_failures": investigated_count,
            "investigated_failure_run_ids": investigated_ids,
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
            "log_max_bytes": bounds.log_max_bytes,
            "checkout_log_reads": checkout_log_reads,
            "max_checkout_log_reads": bounds.max_checkout_log_reads,
            "retired_refs": retired.iter().collect::<Vec<_>>(),
            "max_retired_ref_probes": bounds.max_retired_ref_probes,
            "investigation_cursor": bounds.investigation_cursor,
            "notes": notes,
        }),
        "collected_at": chrono::Utc::now().to_rfc3339(),
    }))
}

/// Work out which heads to scan.
///
/// The integration branch comes from the run's own base branch — the branch
/// this workspace actually ships onto — and the release branch from what
/// GitHub reports as the repository default. Neither is guessed from a naming
/// convention, and when the two coincide the ref is scanned once.
fn derive_refs<Q: CiQueries + ?Sized>(
    queries: &Q,
    input: &Value,
    default_branch: Option<&str>,
    bounds: &Bounds,
    notes: &mut Vec<String>,
    retryable_errors: &mut Vec<Value>,
) -> Result<Vec<ScannedRef>, OrbitError> {
    let integration = optional_input_string(input, "integration_branch")
        .or_else(|| optional_input_string(input, "base_branch"))
        .or_else(|| default_branch.map(ToOwned::to_owned));
    let mut refs: Vec<ScannedRef> = Vec::new();

    for (kind, branch) in [
        (RefKind::Integration, integration),
        (RefKind::Release, default_branch.map(ToOwned::to_owned)),
    ] {
        let Some(branch) = branch else {
            notes.push(format!(
                "no {} branch could be derived; that head was not scanned",
                kind.as_str()
            ));
            continue;
        };
        if refs.iter().any(|scanned| scanned.branch == branch) {
            continue;
        }
        let head_sha = match queries.remote_branch_head(&branch) {
            Ok(head) => head,
            Err(error) => {
                push_retryable_error(
                    retryable_errors,
                    "discovery",
                    "remote_branch_head",
                    None,
                    &format!("branch {branch}: {error}"),
                );
                None
            }
        };
        if head_sha.is_none() {
            notes.push(format!(
                "{} branch '{branch}' has no head on origin; failures there cannot be \
                 compared against a current head",
                kind.as_str()
            ));
        }
        refs.push(ScannedRef {
            kind,
            branch,
            head_sha,
            pr_number: None,
            pr_url: None,
        });
    }

    match queries.open_pull_requests(bounds.max_pull_requests) {
        Ok(pull_requests) => {
            if pull_requests.len() as u64 == bounds.max_pull_requests {
                notes.push(format!(
                    "open pull requests were listed at the cap ({}); further open \
                     pull-request heads may exist and were not scanned",
                    bounds.max_pull_requests
                ));
            }
            for pull_request in pull_requests {
                let Some(branch) = pull_request
                    .get("head_branch")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
                    continue;
                };
                // A head already scanned as the integration or release branch
                // would otherwise be listed twice, reporting each of its
                // failures twice.
                if refs.iter().any(|scanned| scanned.branch == branch) {
                    continue;
                }
                refs.push(ScannedRef {
                    kind: RefKind::PullRequest,
                    branch: branch.to_string(),
                    head_sha: pull_request
                        .get("reported_head_sha")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    pr_number: pull_request.get("number").cloned(),
                    pr_url: pull_request.get("url").cloned(),
                });
            }
        }
        Err(error) => {
            push_retryable_error(
                retryable_errors,
                "discovery",
                "pr_list",
                None,
                &error.to_string(),
            );
            notes.push(
                "open pull requests could not be listed; no pull-request head was scanned"
                    .to_string(),
            );
        }
    }

    Ok(refs)
}

/// Branches that carry a red run but no longer exist on origin.
///
/// A task branch is deleted when its pull request merges, so its old red runs
/// describe code that either landed — where the landing branch's own runs are
/// the current evidence — or was abandoned. Either way there is no ref left to
/// fix, and treating those runs as current is what let a backlog of merged
/// historical pull requests consume every sweep's investigation budget.
///
/// The probe is authoritative (origin, not a naming convention) and bounded:
/// one query per distinct branch that actually carries a red run, and none at
/// all for a branch already scanned as a landing head or an open pull request.
/// A probe that fails leaves its branch alone — a failure to reach origin is
/// never evidence that a failure is resolved.
fn retired_branches<Q: CiQueries + ?Sized>(
    queries: &Q,
    refs: &[ScannedRef],
    runs: &[Value],
    bounds: &Bounds,
    notes: &mut Vec<String>,
) -> std::collections::BTreeSet<String> {
    let mut candidates: Vec<&str> = Vec::new();
    for run in runs {
        let branch = run_branch(run);
        if branch.is_empty() || !run_is_completed(run) || !run_is_unsuccessful(run) {
            continue;
        }
        if refs.iter().any(|scanned| scanned.branch == branch) || candidates.contains(&branch) {
            continue;
        }
        candidates.push(branch);
    }

    let mut retired = std::collections::BTreeSet::new();
    for (probes, branch) in candidates.iter().enumerate() {
        if probes >= bounds.max_retired_ref_probes {
            notes.push(format!(
                "{} branch(es) carrying red runs were not probed against origin \
                 (max_retired_ref_probes={}); their failures stay listed as current rather than \
                 being assumed merged",
                candidates.len() - probes,
                bounds.max_retired_ref_probes
            ));
            break;
        }
        match queries.remote_branch_head(branch) {
            Ok(None) => {
                retired.insert((*branch).to_string());
            }
            Ok(Some(_)) => {}
            Err(error) => notes.push(format!(
                "branch '{branch}' could not be checked against origin ({error}); its failures \
                 stay listed as current"
            )),
        }
    }
    retired
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
fn investigation_slots(
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

fn head_json(scanned: &ScannedRef) -> Value {
    json!({
        "kind": scanned.kind.as_str(),
        "branch": scanned.branch,
        "current_head_sha": scanned.head_sha,
        "pr_number": scanned.pr_number,
        "pr_url": scanned.pr_url,
    })
}

/// Where each run lands once it has been classified.
#[derive(Default)]
struct RunPartition {
    latest: Vec<Value>,
    current: Vec<Value>,
    stale: Vec<Value>,
    in_flight: Vec<Value>,
    mixed_candidates: Vec<Value>,
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
fn partition_runs(
    refs: &[ScannedRef],
    runs: &[Value],
    retired: &std::collections::BTreeSet<String>,
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
                out.current.push(run_summary(ref_for_run(refs, run), run));
                seen_current = true;
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

fn run_branch(run: &Value) -> &str {
    run.get("head_branch").and_then(Value::as_str).unwrap_or("")
}

fn run_is_completed(run: &Value) -> bool {
    run.get("status").and_then(Value::as_str) == Some("completed")
}

fn run_is_unsuccessful(run: &Value) -> bool {
    unsuccessful_conclusion(run.get("conclusion").and_then(Value::as_str))
}

fn has_failed_jobs(failure: &Value) -> bool {
    failure
        .get("failed_jobs")
        .and_then(Value::as_array)
        .is_some_and(|jobs| !jobs.is_empty())
}

fn is_actionable_current_failure(failure: &Value) -> bool {
    if run_is_completed(failure) {
        return run_is_unsuccessful(failure);
    }
    has_failed_jobs(failure) && failure.get("investigated").and_then(Value::as_bool) == Some(true)
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

/// Integration first, then release, then pull requests; newest run first
/// within each. Investigation budget therefore lands on the heads that gate
/// delivery before it lands on a pull request.
fn sort_current_failures(failures: &mut [Value]) {
    let rank = |value: &Value| match value.get("ref_kind").and_then(Value::as_str) {
        Some("integration") => 0,
        Some("release") => 1,
        _ => 2,
    };
    failures.sort_by(|left, right| {
        rank(left)
            .cmp(&rank(right))
            .then_with(|| run_order(right).cmp(&run_order(left)))
    });
}

/// Fill one current failure in with its failed jobs, steps, bounded log, and
/// the commit its runner actually checked out.
fn investigate<Q: CiQueries + ?Sized>(
    queries: &Q,
    failure: &mut Value,
    bounds: &Bounds,
    checkout_log_reads: &mut usize,
    retryable_errors: &mut Vec<Value>,
) {
    let Some(run_id) = failure
        .get("run_id")
        .and_then(Value::as_u64)
        .map(|id| id.to_string())
    else {
        push_retryable_error(
            retryable_errors,
            "registration",
            "run_identity",
            None,
            "current failure has no numeric run_id",
        );
        return;
    };
    let errors_before = retryable_errors.len();

    match queries.run_view(&run_id) {
        Ok(view) => {
            let failed_jobs = view.get("failed_jobs").cloned().unwrap_or(json!([]));
            let empty = failed_jobs.as_array().is_none_or(Vec::is_empty);
            if empty && run_is_completed(failure) {
                push_retryable_error(
                    retryable_errors,
                    "registration",
                    "run_view",
                    failure.get("run_id"),
                    "failed run returned no failed jobs",
                );
            }
            failure["failed_jobs"] = failed_jobs;
            // A pending in-flight check is not a repair task. Do not fetch
            // logs or consume checkout budget until a job has actually failed.
            if empty && !run_is_completed(failure) {
                failure["investigated"] = json!(true);
                return;
            }
        }
        Err(error) => {
            push_retryable_error(
                retryable_errors,
                "investigation",
                "run_view",
                failure.get("run_id"),
                &error.to_string(),
            );
            if !run_is_completed(failure) {
                failure["investigated"] = json!(false);
                return;
            }
        }
    }

    match queries.run_logs(&run_id, LogScope::Failed, bounds.log_max_bytes) {
        Ok(log) => {
            failure["log_excerpt"] = json!(log.text);
            failure["log_truncated"] = json!(log.truncated);
            failure["log_total_bytes"] = json!(log.total_bytes);
            failure["log_returned_bytes"] = json!(log.returned_bytes);
            failure["log_scope"] = json!("failed");
            failure["actual_checkout_shas"] = json!(log.checkout_commits);
            failure["checkout_evidence"] = json!(log.checkout_evidence);
            failure["checkout_evidence_scope"] = json!("failed");
            set_checkout_identity(failure, "failed", &log);
            // `gh` can succeed with empty stdout when the run's logs are gone
            // (retention). That is not a captured excerpt; record it so the
            // filed task can say why the block is empty.
            if log.text.trim().is_empty() {
                push_retryable_error(
                    retryable_errors,
                    "investigation",
                    "run_logs",
                    failure.get("run_id"),
                    "query returned no failed-step log text",
                );
            }
        }
        Err(error) => {
            push_retryable_error(
                retryable_errors,
                "investigation",
                "run_logs",
                failure.get("run_id"),
                &error.to_string(),
            );
        }
    }

    // The checkout step normally succeeds, so it is absent from the
    // failed-step log. One full-log read per run, within a hard budget,
    // recovers the commit under test; past the budget we say so rather than
    // leaving the field silently empty.
    let needs_checkout = failure
        .get("actual_checkout_shas")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
        || failure
            .get("checkout_evidence_complete")
            .and_then(Value::as_bool)
            != Some(true);
    if !needs_checkout {
        failure["investigated"] = json!(retryable_errors.len() == errors_before);
        return;
    }
    if *checkout_log_reads >= bounds.max_checkout_log_reads {
        failure["checkout_evidence_scope"] = json!("skipped_budget_exhausted");
        push_retryable_error(
            retryable_errors,
            "investigation",
            "checkout_evidence_budget",
            failure.get("run_id"),
            "actual checkout SHA was not collected because max_checkout_log_reads was exhausted",
        );
        failure["investigated"] = json!(false);
        return;
    }
    *checkout_log_reads += 1;
    match queries.run_logs(&run_id, LogScope::All, bounds.log_max_bytes) {
        Ok(log) => {
            failure["actual_checkout_shas"] = json!(log.checkout_commits);
            failure["checkout_evidence"] = json!(log.checkout_evidence);
            failure["checkout_evidence_scope"] = json!("all");
            set_checkout_identity(failure, "all", &log);
            // A genuinely incomplete scan (the source-byte cap, or a dropped
            // overlong line that could have carried checkout identity) stays
            // fail-closed even when one SHA was already found: it cannot rule
            // out a later, conflicting identity past whatever it didn't
            // manage to read. Only a *display* cap (evidence line/commit
            // count, or an overlong line unrelated to checkout) is exempt —
            // that never touches `checkout_evidence_complete`.
            if !log.checkout_evidence_complete {
                push_retryable_error(
                    retryable_errors,
                    "registration",
                    "checkout_evidence",
                    failure.get("run_id"),
                    "checkout evidence scan reached its hard limit; actual checkout identity is incomplete",
                );
            } else if log.checkout_commits.is_empty() {
                push_retryable_error(
                    retryable_errors,
                    "registration",
                    "checkout_evidence",
                    failure.get("run_id"),
                    "run logs contained no actual checkout SHA",
                );
            }
        }
        Err(error) => {
            failure["checkout_evidence_scope"] = json!("unavailable");
            push_retryable_error(
                retryable_errors,
                "investigation",
                "run_logs_all",
                failure.get("run_id"),
                &error.to_string(),
            );
        }
    }
    failure["investigated"] = json!(retryable_errors.len() == errors_before);
}

fn set_checkout_identity(failure: &mut Value, scope: &str, log: &super::query::RunLog) {
    let state = if !log.checkout_evidence_complete {
        "incomplete"
    } else {
        match log.checkout_commits.len() {
            0 => "missing",
            1 => "observed",
            _ => "ambiguous",
        }
    };
    failure["checkout_evidence_complete"] = json!(log.checkout_evidence_complete);
    failure["checkout_evidence_scanned_bytes"] = json!(log.checkout_evidence_scanned_bytes);
    failure["checkout_evidence_source_truncated"] = json!(log.checkout_evidence_source_truncated);
    failure["checkout_evidence_display_truncated"] = json!(log.checkout_evidence_display_truncated);
    failure["checkout_identity"] = json!({
        "state": state,
        "observed_shas": log.checkout_commits,
        "provenance": {
            "source": "runner_log",
            "scope": scope,
            "complete": log.checkout_evidence_complete,
            "scanned_bytes": log.checkout_evidence_scanned_bytes,
            "source_truncated": log.checkout_evidence_source_truncated,
            // Display caps (evidence line/commit count, or an unrelated
            // overlong line) reduce what is reported without bearing on
            // whether identity itself was captured; see `complete` for that.
            "display_truncated": log.checkout_evidence_display_truncated,
        },
    });
}

fn push_retryable_error(
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
