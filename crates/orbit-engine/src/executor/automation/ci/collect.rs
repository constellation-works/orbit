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
use super::query::CiQueries;
use super::{
    OUTCOME_CAPABILITY_UNAVAILABLE, OUTCOME_CURRENT_FAILURES, OUTCOME_NO_CURRENT_FAILURE,
    OUTCOME_RETRYABLE_ERROR, bounded_u64, optional_input_string, unsuccessful_conclusion,
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

pub(super) struct Bounds {
    max_runs: u64,
    max_pull_requests: u64,
    max_investigated_runs: usize,
    pub(super) log_max_bytes: usize,
    pub(super) max_checkout_log_reads: usize,
    pub(super) max_job_log_reads: usize,
    max_retired_ref_probes: usize,
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
    let refs = derive_refs(
        queries,
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
    let probes = probe_branches(queries, &refs, &runs, &bounds, &mut notes);
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

struct CandidateProbeResults {
    retired: std::collections::BTreeSet<String>,
    unverified: std::collections::BTreeMap<String, (String, String)>,
}

/// Branches that carry a red run but no longer exist on origin, or whose
/// current relevance could not be verified within probe bounds.
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
/// A probe that fails or is skipped due to probe budget keeps its branch
/// deferred rather than assuming it is merged or current.
fn probe_branches<Q: CiQueries + ?Sized>(
    queries: &Q,
    refs: &[ScannedRef],
    runs: &[Value],
    bounds: &Bounds,
    notes: &mut Vec<String>,
) -> CandidateProbeResults {
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

    let selected = probe_slots(
        candidates.len(),
        bounds.max_retired_ref_probes,
        bounds.investigation_cursor,
    );

    if candidates.len() > selected.len() {
        notes.push(format!(
            "{} branch(es) carrying red runs were not probed against origin \
             (max_retired_ref_probes={}); their failures remain deferred until verified",
            candidates.len() - selected.len(),
            bounds.max_retired_ref_probes
        ));
    }

    let mut retired = std::collections::BTreeSet::new();
    let mut unverified = std::collections::BTreeMap::new();

    for (index, branch) in candidates.iter().enumerate() {
        if !selected.contains(&index) {
            unverified.insert(
                (*branch).to_string(),
                (
                    "retired_ref_budget".to_string(),
                    format!(
                        "candidate branch '{branch}' was not probed against origin because \
                         max_retired_ref_probes ({}) was exhausted; its failure remains deferred \
                         until verified",
                        bounds.max_retired_ref_probes
                    ),
                ),
            );
            continue;
        }

        match queries.remote_branch_head(branch) {
            Ok(None) => {
                retired.insert((*branch).to_string());
            }
            Ok(Some(_)) => {}
            Err(error) => {
                notes.push(format!(
                    "branch '{branch}' could not be checked against origin ({error}); its \
                     failure remains deferred until verified"
                ));
                unverified.insert(
                    (*branch).to_string(),
                    (
                        "remote_branch_head".to_string(),
                        format!(
                            "candidate branch '{branch}' could not be checked against origin \
                             ({error}); its failure remains deferred until verified"
                        ),
                    ),
                );
            }
        }
    }

    CandidateProbeResults {
        retired,
        unverified,
    }
}

/// Which candidate branches this sweep probes against origin.
///
/// If candidate count exceeds the probe budget, the prefix of slots probes the
/// newest candidates while the final slot rotates through overflow candidates
/// with each advancing cursor so all candidates eventually get probed without
/// starvation.
fn probe_slots(candidates: usize, budget: usize, cursor: u64) -> std::collections::BTreeSet<usize> {
    let attempted = candidates.min(budget);
    let mut slots: std::collections::BTreeSet<usize> = (0..attempted).collect();
    if candidates <= attempted || attempted == 0 {
        return slots;
    }
    let rotating = attempted - 1;
    slots.remove(&rotating);
    let overflow = candidates - rotating;
    slots.insert(rotating + (cursor % overflow as u64) as usize);
    slots
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
    deferred: Vec<Value>,
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

fn run_branch(run: &Value) -> &str {
    run.get("head_branch").and_then(Value::as_str).unwrap_or("")
}

pub(super) fn run_is_completed(run: &Value) -> bool {
    run.get("status").and_then(Value::as_str) == Some("completed")
}

fn run_is_unsuccessful(run: &Value) -> bool {
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

fn is_actionable_current_failure(failure: &Value) -> bool {
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
fn supersede_older_when_cancelled_run_is_actionable(
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
fn sort_current_failures(failures: &mut [Value]) {
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
