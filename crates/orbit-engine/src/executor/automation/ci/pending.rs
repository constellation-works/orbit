//! Red runs held back while a newer push run of the same workflow and branch is
//! still running on a descendant commit.
//!
//! Such a run may already carry the repair, so a lone red run is held: nothing
//! is filed for it this sweep, and it is listed in `pending_supersession` with
//! both run ids. On a busy landing branch a newer run is almost always in
//! flight, so the hold is narrow and bounded:
//!
//! - A red run whose previous completed run of the same workflow and branch,
//!   at an ancestor commit, failed the same job with the same normalized error
//!   signature has reproduced. It stays current with `reproduced_on` naming
//!   that run. Sharing a job and step is not enough: one CI step can run
//!   fmt, clippy, doc and tests, so consecutive reds there may have unrelated
//!   causes. The signature is the one CI failure filing dedupes on
//!   ([`super::log_signature::error_signature`]); a step-name fallback names
//!   no cause, so it never proves a reproduction.
//! - A red run held for longer than the window stays current with
//!   `held_past_window` naming the run it waited on.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::investigate::{bound_diagnostic, log_belongs_to_job};
use super::log_signature::{ErrorSignature, error_signature};
use super::partition::{run_is_cancelled, run_is_unsuccessful};
use super::query::{CiQueries, LogScope};

/// Why a current failure was deferred rather than filed.
pub(super) const IN_FLIGHT_DESCENDANT_REASON: &str = "newer_descendant_run_in_flight";

/// Bound on ancestry checks per sweep. Each is a local Git query unless the
/// checkout first has to fetch a run commit it has not seen.
const MAX_ANCESTRY_CHECKS: usize = 25;

/// Bound on run views read to compare a held run's failed jobs with its
/// previous completed run's. Each comparison reads two.
const MAX_REPRODUCTION_VIEW_READS: usize = 12;

/// Bound on failed-step log reads taken to compare a held run's error
/// signature with its previous completed run's. Each shared failed job costs
/// two, and only runs that already share a failed job and step are read.
const MAX_REPRODUCTION_LOG_READS: usize = 6;

/// What collection knows about each current failure's neighbours.
pub(super) struct Supersession<'a> {
    /// Newer in-flight push runs of the same workflow and branch, per run id.
    pub(super) successors: &'a BTreeMap<u64, Vec<Value>>,
    /// The next older completed run of the same workflow and branch, per run id.
    pub(super) previous_completed: &'a BTreeMap<u64, Value>,
    /// How long a failure may be held before it is filed anyway.
    pub(super) window_minutes: u64,
    /// Byte bound on each failed-step log read for a signature comparison.
    pub(super) log_max_bytes: usize,
    pub(super) now: DateTime<Utc>,
}

/// Current failures after the hold.
pub(super) struct Held {
    pub(super) current: Vec<Value>,
    pub(super) pending: Vec<Value>,
    /// Run views fetched for a comparison, keyed by run id, so investigation
    /// does not read them again.
    pub(super) views: BTreeMap<u64, Value>,
}

/// Move each current failure with an in-flight push successor at a descendant
/// commit out of `current`, unless it reproduced on its previous completed run
/// or has been held past the window. A failure whose ancestry cannot be
/// established stays current, as before, and a note says why.
pub(super) fn hold_for_in_flight_descendants<Q: CiQueries + ?Sized>(
    queries: &Q,
    current: Vec<Value>,
    supersession: &Supersession<'_>,
    notes: &mut Vec<String>,
) -> Held {
    let mut held = Held {
        current: Vec::new(),
        pending: Vec::new(),
        views: BTreeMap::new(),
    };
    let mut budget = Budget::default();
    let mut unchecked = 0usize;
    let mut reproduced = 0usize;
    let mut expired = 0usize;
    for mut failure in current {
        let newer = failure
            .get("run_id")
            .and_then(Value::as_u64)
            .and_then(|run_id| supersession.successors.get(&run_id));
        let commit = failure
            .get("event_reported_head_sha")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let (Some(newer), Some(commit)) = (newer, commit) else {
            held.current.push(failure);
            continue;
        };
        let mut descendant = None;
        for successor in newer {
            let Some(sha) = successor.get("reported_head_sha").and_then(Value::as_str) else {
                continue;
            };
            if budget.ancestry_checks == MAX_ANCESTRY_CHECKS {
                unchecked += 1;
                break;
            }
            budget.ancestry_checks += 1;
            match queries.is_ancestor(&commit, sha) {
                Ok(true) => {
                    descendant = Some(successor);
                    break;
                }
                Ok(false) => {}
                Err(error) => notes.push(format!(
                    "run {} could not be compared with in-flight run {} ({error}); the failure \
                     stays current",
                    display_id(&failure),
                    display_id(successor),
                )),
            }
        }
        let Some(successor) = descendant else {
            held.current.push(failure);
            continue;
        };
        let since = pending_since(&failure);
        let window = chrono::Duration::minutes(supersession.window_minutes as i64);
        if let Some(since) = since.filter(|since| supersession.now - *since >= window) {
            failure["held_past_window"] = json!({
                "pending_on": pending_on(successor),
                "pending_since": since.to_rfc3339(),
                "window_minutes": supersession.window_minutes,
            });
            held.current.push(failure);
            expired += 1;
            continue;
        }
        match reproduction(
            queries,
            &failure,
            &commit,
            supersession,
            &mut budget,
            &mut held.views,
            notes,
        ) {
            Some(mut previous) => {
                previous["pending_on"] = pending_on(successor);
                failure["reproduced_on"] = previous;
                held.current.push(failure);
                reproduced += 1;
            }
            None => held.pending.push(pending_entry(
                failure,
                successor,
                &commit,
                since,
                supersession.window_minutes,
            )),
        }
    }
    if unchecked > 0 {
        notes.push(format!(
            "{unchecked} current failure(s) with an in-flight successor were not checked for \
             ancestry (cap {MAX_ANCESTRY_CHECKS}) and stay current"
        ));
    }
    if budget.comparisons_skipped > 0 {
        notes.push(format!(
            "{} held failure(s) were not compared with their previous completed run (caps \
             {MAX_ANCESTRY_CHECKS} ancestry checks, {MAX_REPRODUCTION_VIEW_READS} run views, \
             {MAX_REPRODUCTION_LOG_READS} failed-step logs); \
             they stay held until the window expires or a later sweep compares them",
            budget.comparisons_skipped
        ));
    }
    if reproduced > 0 {
        notes.push(format!(
            "{reproduced} current failure(s) have a newer push run in flight at a descendant \
             commit but already failed the same job with the same normalized error signature on \
             the previous completed run, so they are filed rather than held; each names that run \
             in reproduced_on"
        ));
    }
    if expired > 0 {
        notes.push(format!(
            "{expired} current failure(s) were held for at least {} minutes while a newer push \
             run was in flight at a descendant commit, so they are filed anyway; each names the \
             run it waited on in held_past_window",
            supersession.window_minutes
        ));
    }
    if !held.pending.is_empty() {
        notes.push(format!(
            "{} current failure(s) are held in pending_supersession because a newer push run of \
             the same workflow on the same branch is still running at a descendant commit and \
             the failure has not reproduced on a previous completed run; each is filed once it \
             reproduces or after {} minutes",
            held.pending.len(),
            supersession.window_minutes
        ));
    }
    held
}

#[derive(Default)]
struct Budget {
    ancestry_checks: usize,
    view_reads: usize,
    log_reads: usize,
    comparisons_skipped: usize,
}

/// The previous completed run this red run reproduced on: red itself, at the
/// same or an ancestor commit, failing a job they share, at a step they share,
/// with the same concrete normalized error signature.
fn reproduction<Q: CiQueries + ?Sized>(
    queries: &Q,
    failure: &Value,
    commit: &str,
    supersession: &Supersession<'_>,
    budget: &mut Budget,
    views: &mut BTreeMap<u64, Value>,
    notes: &mut Vec<String>,
) -> Option<Value> {
    let run_id = failure.get("run_id").and_then(Value::as_u64)?;
    let previous = supersession.previous_completed.get(&run_id)?;
    // A cancellation is not a reproduction: it may have been interrupted
    // before reaching the failing step.
    if !run_is_unsuccessful(previous) || run_is_cancelled(previous) {
        return None;
    }
    let previous_id = previous.get("run_id").and_then(Value::as_u64)?;
    let previous_commit = previous.get("reported_head_sha").and_then(Value::as_str)?;
    if previous_commit != commit {
        if budget.ancestry_checks == MAX_ANCESTRY_CHECKS {
            budget.comparisons_skipped += 1;
            return None;
        }
        budget.ancestry_checks += 1;
        match queries.is_ancestor(previous_commit, commit) {
            Ok(true) => {}
            Ok(false) => return None,
            Err(error) => {
                notes.push(format!(
                    "run {run_id} could not be compared with its previous completed run \
                     {previous_id} ({error}); it stays held"
                ));
                return None;
            }
        }
    }
    let wanted = [run_id, previous_id]
        .into_iter()
        .filter(|id| !views.contains_key(id))
        .count();
    if budget.view_reads + wanted > MAX_REPRODUCTION_VIEW_READS {
        budget.comparisons_skipped += 1;
        return None;
    }
    for id in [run_id, previous_id] {
        if views.contains_key(&id) {
            continue;
        }
        budget.view_reads += 1;
        match queries.run_view(&id.to_string()) {
            Ok(view) => {
                views.insert(id, view);
            }
            Err(error) => {
                notes.push(format!(
                    "run view {id} could not be read to compare run {run_id} with its previous \
                     completed run ({error}); it stays held"
                ));
                return None;
            }
        }
    }
    let held_view = views.get(&run_id)?;
    let previous_view = views.get(&previous_id)?;
    let shared_steps = failed_steps(held_view)
        .intersection(&failed_steps(previous_view))
        .cloned()
        .collect::<BTreeSet<_>>();
    let shared_jobs = shared_steps
        .iter()
        .map(|(job, _)| job.as_str())
        .collect::<BTreeSet<_>>();
    for job in shared_jobs {
        let (Some(held_job), Some(previous_job)) =
            (failed_job(held_view, job), failed_job(previous_view, job))
        else {
            continue;
        };
        if budget.log_reads + 2 > MAX_REPRODUCTION_LOG_READS {
            budget.comparisons_skipped += 1;
            return None;
        }
        let held = job_signature(
            queries,
            run_id,
            held_job,
            held_view,
            supersession,
            budget,
            notes,
        )?;
        let before = job_signature(
            queries,
            previous_id,
            previous_job,
            previous_view,
            supersession,
            budget,
            notes,
        )?;
        let (Some(held), Some(before)) = (held, before) else {
            continue;
        };
        if held.step_fallback || before.step_fallback || held.text != before.text {
            continue;
        }
        let steps = shared_steps
            .iter()
            .filter(|(name, _)| name == job)
            .map(|(_, step)| step.as_str())
            .collect::<Vec<_>>();
        return Some(json!({
            "run_id": previous_id,
            "url": previous.get("url"),
            "created_at": previous.get("created_at"),
            "conclusion": previous.get("conclusion"),
            "event_reported_head_sha": previous_commit,
            "shared_cause": {
                "job": job,
                "steps": steps,
                "normalized_error_signature": held.text,
            },
        }));
    }
    None
}

/// The first non-cancelled failed job with this name and a numeric id.
fn failed_job<'a>(view: &'a Value, name: &str) -> Option<&'a Value> {
    view.get("failed_jobs")
        .and_then(Value::as_array)?
        .iter()
        .filter(|job| job.get("conclusion").and_then(Value::as_str) != Some("cancelled"))
        .find(|job| {
            job.get("name").and_then(Value::as_str) == Some(name)
                && job.get("job_id").and_then(Value::as_u64).is_some()
        })
}

/// One failed job's normalized error signature, read from its failed-step log
/// the way filing reads it: the bound runner diagnostic when there is one,
/// else the log excerpt, keyed by the job's first failed step. The outer
/// `None` means the log could not be read (noted; the run stays held); the
/// inner `None` that the log is another job's and proves nothing.
fn job_signature<Q: CiQueries + ?Sized>(
    queries: &Q,
    run_id: u64,
    job: &Value,
    view: &Value,
    supersession: &Supersession<'_>,
    budget: &mut Budget,
    notes: &mut Vec<String>,
) -> Option<Option<ErrorSignature>> {
    let job_id = job.get("job_id").and_then(Value::as_u64)?;
    budget.log_reads += 1;
    let log = match queries.run_logs(
        &run_id.to_string(),
        job_id,
        LogScope::Failed,
        supersession.log_max_bytes,
        Some(view),
    ) {
        Ok(log) => log,
        Err(error) => {
            notes.push(format!(
                "failed-step log of job {job_id} in run {run_id} could not be read to compare \
                 error signatures ({error}); the run it would release stays held"
            ));
            return None;
        }
    };
    if !log_belongs_to_job(&log, job_id) {
        return Some(None);
    }
    let step = job
        .get("failed_steps")
        .and_then(Value::as_array)
        .and_then(|steps| steps.first())
        .and_then(|step| step.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let diagnostic = bound_diagnostic(&log, &json!({"failed_jobs": [job]}), job_id);
    let text = diagnostic
        .as_ref()
        .and_then(|unit| unit.get("text"))
        .and_then(Value::as_str)
        .unwrap_or(&log.text);
    Some(Some(error_signature(text, step)))
}

/// Each failed job's name paired with each of its failed steps' names. A job
/// that lists no failed step contributes nothing: there is no cause to match.
fn failed_steps(view: &Value) -> BTreeSet<(String, String)> {
    view.get("failed_jobs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|job| job.get("conclusion").and_then(Value::as_str) != Some("cancelled"))
        .flat_map(|job| {
            let name = job
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            job.get("failed_steps")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|step| step.get("name").and_then(Value::as_str))
                .map(move |step| (name.clone(), step.to_string()))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// When the failure became fileable: its run's completion, which GitHub
/// reports as the run's last update, else its creation.
fn pending_since(failure: &Value) -> Option<DateTime<Utc>> {
    ["updated_at", "created_at"].into_iter().find_map(|field| {
        failure
            .get(field)
            .and_then(Value::as_str)
            .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
            .map(|time| time.with_timezone(&Utc))
    })
}

fn pending_entry(
    mut failure: Value,
    successor: &Value,
    commit: &str,
    since: Option<DateTime<Utc>>,
    window_minutes: u64,
) -> Value {
    failure["reason"] = json!(IN_FLIGHT_DESCENDANT_REASON);
    failure["evidence"] = json!(format!(
        "newer push run {} of workflow '{}' on branch '{}' is {} at {}, a descendant of the \
         failing commit {commit}; this failure is filed once it reproduces on a completed run, \
         or after it has been held for {window_minutes} minutes",
        display_id(successor),
        failure
            .get("workflow")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        failure
            .get("head_branch")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        successor
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("not completed"),
        sha_of(successor),
    ));
    failure["pending_on"] = pending_on(successor);
    failure["pending_since"] = json!(since.map(|since| since.to_rfc3339()));
    failure["window_minutes"] = json!(window_minutes);
    failure
}

fn pending_on(successor: &Value) -> Value {
    json!({
        "run_id": successor.get("run_id"),
        "url": successor.get("url"),
        "created_at": successor.get("created_at"),
        "status": successor.get("status"),
        "event": successor.get("event"),
        "reported_head_sha": sha_of(successor),
    })
}

fn sha_of(run: &Value) -> &str {
    run.get("reported_head_sha")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn display_id(run: &Value) -> String {
    run.get("run_id")
        .and_then(Value::as_u64)
        .map_or_else(|| "unknown".to_string(), |id| id.to_string())
}
