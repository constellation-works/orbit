//! A stale base result cannot attribute an untouched-file failure to a PR.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskAutomationUpdate};

use super::super::super::super::ci::log_signature;
use super::super::super::{
    git::{git_output, git_output_paths},
    operations,
};
use super::{annotations, jobs};

/// Whether an untouched-path failure needs same-check base and PR-rerun evidence.
pub(super) enum UntouchedBaseEvidence {
    Unrelated,
    Waiting(String),
    PassingBaseRun,
}

/// Independently guard the repair evidence path, including job-level recovery.
#[allow(clippy::too_many_arguments)]
pub(super) fn watcher_suspicion<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    tasks: &[String],
    head: &str,
    base_branch: Option<&str>,
    job: &Value,
    log: &str,
    signature: &str,
) -> Result<Option<String>, String> {
    let paths = repository_citations(workspace, head, log)?;
    if paths.is_empty() {
        return Ok(None);
    }
    let branch = base_branch.ok_or("the failed check's base branch could not be established")?;
    let branch = branch.strip_prefix("origin/").unwrap_or(branch);
    let tip = git_output(
        workspace,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/remotes/origin/{branch}^{{commit}}"),
        ],
    )
    .or_else(|_| {
        git_output(
            workspace,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{branch}^{{commit}}"),
            ],
        )
    })
    .map_err(|error| format!("the failed check's base could not be read: {error}"))?;
    let base = git_output(workspace, &["merge-base", head, tip.trim()])
        .map_err(|error| error.to_string())?;
    untouched_base_evidence(
        host,
        workspace,
        head,
        base.trim(),
        tip.trim(),
        branch,
        job,
        paths,
    )
    .map(|evidence| match evidence {
        UntouchedBaseEvidence::Waiting(reason) => Some(reason),
        UntouchedBaseEvidence::PassingBaseRun
            if !has_matching_candidate_rerun(
                host,
                tasks,
                head,
                job,
                &jobs::job_label(job),
                signature,
            ) =>
        {
            Some(format!(
                "base-suspect: '{}' has a successful same-check base run, but head {head} \
                 has no unchanged-head pull_request rerun with the same failure signature; \
                 a base pass alone does not authorize repair",
                jobs::job_label(job),
            ))
        }
        UntouchedBaseEvidence::Unrelated | UntouchedBaseEvidence::PassingBaseRun => None,
    })
}

/// The repository files a log cites at `head`. A relative citation must
/// name a blob there. An absolute one, such as a runner's checkout path,
/// names the longest of its trailing component paths that is a blob there;
/// a citation outside the checkout that happens to match one only delays a
/// repair until the base's fresh run decides.
fn repository_citations(
    workspace: &Path,
    head: &str,
    log: &str,
) -> Result<BTreeSet<String>, String> {
    let cited = log_signature::cited_location_paths(log);
    if cited.is_empty() {
        return Ok(BTreeSet::new());
    }
    let blobs = git_output_paths(
        workspace,
        &["ls-tree", "-r", "-z", "--name-only", "--full-tree", head],
    )
    .map_err(|error| format!("the files of head {head} could not be listed: {error}"))?
    .into_iter()
    .collect::<BTreeSet<_>>();
    let in_repository = |path: &str| blobs.contains(path);
    Ok(cited
        .iter()
        .filter_map(|path| match path.strip_prefix('/') {
            None => in_repository(path).then_some(path.as_str()),
            Some(absolute) => absolute
                .match_indices('/')
                .map(|(at, _)| &absolute[at + 1..])
                .find(|suffix| in_repository(suffix)),
        })
        .map(str::to_string)
        .collect())
}

/// Whether a matching base check passes. A pass alone only authorizes one
/// unchanged-head pull-request rerun; only a repeated signature on that
/// rerun can attribute the failure to the candidate.
#[allow(clippy::too_many_arguments)]
pub(super) fn stale_untouched_failure<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    head: &str,
    base: &str,
    tip: &str,
    base_branch: &str,
    job: &Value,
    log: &str,
) -> Result<UntouchedBaseEvidence, String> {
    let paths = repository_citations(workspace, head, log)?;
    untouched_base_evidence(host, workspace, head, base, tip, base_branch, job, paths)
}

/// Whether `log` cites only repository files the candidate did not change
/// from `base` to `head`: the precondition for a base-suspect failure.
pub(super) fn is_untouched_failure(
    workspace: &Path,
    head: &str,
    base: &str,
    log: &str,
) -> Result<bool, String> {
    let paths = repository_citations(workspace, head, log)?;
    if paths.is_empty() {
        return Ok(false);
    }
    let changed = annotations::candidate_changed_paths(workspace, base, head)?;
    Ok(paths.is_disjoint(&changed))
}

/// Whether `other` started at or after `job` completed. Missing or
/// unparseable timestamps are never fresh evidence.
pub(super) fn started_after_completion(job: &Value, other: &Value) -> bool {
    let completed = job["completed_at"]
        .as_str()
        .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok());
    let started = other["started_at"]
        .as_str()
        .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok());
    match (completed, started) {
        (Some(completed), Some(started)) => started >= completed,
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn untouched_base_evidence<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    head: &str,
    base: &str,
    tip: &str,
    base_branch: &str,
    job: &Value,
    paths: BTreeSet<String>,
) -> Result<UntouchedBaseEvidence, String> {
    if paths.is_empty() {
        return Ok(UntouchedBaseEvidence::Unrelated);
    }
    let changed = annotations::candidate_changed_paths(workspace, base, head)?;
    if !paths.is_disjoint(&changed) {
        return Ok(UntouchedBaseEvidence::Unrelated);
    }
    let response = host
        .run_private_vcs_operation(
            operations::CI_COMMIT_JOBS,
            json!({"commit":base,"workspace_path":workspace,"latest_push_branch":base_branch,"base_push_tip":tip}),
        )
        .map_err(|error| format!("the base re-check on {tip} could not be read: {error}"))?;
    let base_jobs = response["jobs"]
        .as_array()
        .ok_or("the base re-check carried no job list")?;
    if base_jobs.iter().any(|base_job| {
        jobs::same_job(base_job, job)
            && base_job["run_status"] == "completed"
            && base_job["status"] == "completed"
            && base_job["conclusion"] == "success"
    }) {
        return Ok(UntouchedBaseEvidence::PassingBaseRun);
    }
    Ok(UntouchedBaseEvidence::Waiting(format!(
        "base-suspect: its cited repository files ({}) are outside the candidate's changed paths; \
         waiting for a successful same-check push run of '{}' from base {base} through {tip} and an \
         unchanged-head pull_request rerun with the same failure signature; a base pass alone does not authorize repair",
        paths.into_iter().take(20).collect::<Vec<_>>().join(", "),
        jobs::job_label(job),
    )))
}

/// Whether task history records an earlier failed attempt with this same
/// signature on the same head, followed by the current job starting later.
/// The first failure is retained before the rerun is requested, so a resumed
/// landing or auto-fix can recognize the same evidence as an in-process rerun.
pub(super) fn has_matching_candidate_rerun<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[String],
    head: &str,
    job: &Value,
    check: &str,
    signature: &str,
) -> bool {
    let current_start = job["started_at"]
        .as_str()
        .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok());
    let Some(current_start) = current_start else {
        return false;
    };
    let current_run = job["run_id"].as_u64();
    let current_job = job["job_id"].as_u64();
    let current_attempt = job["run_attempt"].as_u64();
    for task in tasks {
        let Ok(history) = host.get_task_history(task) else {
            continue;
        };
        for entry in history
            .iter()
            .filter(|entry| matches!(entry.event.as_str(), "ci_base_suspect" | "ci_check_rerun"))
        {
            let Some(note) = entry
                .note
                .as_deref()
                .and_then(|note| serde_json::from_str::<Value>(note).ok())
            else {
                continue;
            };
            let prior = if entry.event == "ci_check_rerun" {
                note["jobs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|prior| prior["check"] == check && prior["signature"] == signature)
                    .map(|prior| (note["head"].as_str(), note["run_id"].as_u64(), prior))
            } else {
                (note["check"] == check && note["signature"] == signature).then_some((
                    note["head"].as_str(),
                    note["run_id"].as_u64(),
                    &note,
                ))
            };
            let Some((Some(prior_head), prior_run, prior)) = prior else {
                continue;
            };
            if prior_head != head {
                continue;
            }
            let completed = prior["completed_at"]
                .as_str()
                .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok());
            let Some(completed) = completed else {
                continue;
            };
            if current_start < completed {
                continue;
            }
            let same_identity = current_run == prior_run
                && current_job == prior["job_id"].as_u64()
                && current_attempt == prior["run_attempt"].as_u64();
            if !same_identity {
                return true;
            }
        }
    }
    false
}

/// Surface the base-wide suspicion once per owning task and exact failure,
/// independent of poll count or PR head. This spends no repair reservation.
pub(super) fn record_base_suspect<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[String],
    evidence: &Value,
    handoff_id: Option<&str>,
) -> Result<(), OrbitError> {
    let note = serde_json::to_string(evidence)
        .map_err(|error| OrbitError::Execution(error.to_string()))?;
    for task in tasks {
        if host
            .get_task_history(task)?
            .iter()
            .any(|entry| entry.event == "ci_base_suspect" && entry.note.as_deref() == Some(&note))
        {
            continue;
        }
        if let Some(handoff_id) = handoff_id {
            host.record_handoff_landing_history(handoff_id, "ci_base_suspect", &note)?;
        } else {
            host.apply_task_automation_update(
                task,
                TaskAutomationUpdate {
                    append_history: vec![orbit_types::task::TaskHistoryEntry {
                        at: chrono::Utc::now(),
                        by: "system".into(),
                        event: "ci_base_suspect".into(),
                        note: Some(note.clone()),
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: None,
                    }],
                    ..TaskAutomationUpdate::default()
                },
            )?;
        }
        tracing::warn!(task, %evidence, "base-suspect: waiting for conclusive same-check base and pull-request rerun evidence; no candidate repair");
    }
    Ok(())
}
