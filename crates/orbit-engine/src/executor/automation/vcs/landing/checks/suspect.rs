//! A stale base result cannot attribute an untouched-file failure to a PR.

use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskAutomationUpdate};

use super::super::super::super::ci::log_signature;
use super::super::super::{git::git_output, operations};
use super::{annotations, jobs};

/// Independently guard the repair evidence path, including job-level recovery.
pub(super) fn watcher_suspicion<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    head: &str,
    base_branch: Option<&str>,
    job: &Value,
    log: &str,
) -> Result<Option<String>, String> {
    if repository_citations(workspace, head, log).is_empty() {
        return Ok(None);
    }
    let branch = base_branch.ok_or("the failed check's base branch could not be established")?;
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
    stale_untouched_failure(
        host,
        workspace,
        head,
        base.trim(),
        tip.trim(),
        branch,
        job,
        log,
    )
}

fn repository_citations(
    workspace: &Path,
    head: &str,
    log: &str,
) -> std::collections::BTreeSet<String> {
    log_signature::cited_location_paths(log)
        .into_iter()
        .filter(|path| {
            annotations::source_path(path)
                && git_output(workspace, &["cat-file", "-t", &format!("{head}:{path}")])
                    .is_ok_and(|kind| kind.trim() == "blob")
        })
        .collect()
}

/// Why this failure needs the base's next push run before a candidate
/// repair. A fresh passing job clears the suspicion. Missing timestamps,
/// pending/skipped jobs and failed reads never turn an old green run into
/// fresh evidence. The caller's existing completion deadline bounds polling.
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
) -> Result<Option<String>, String> {
    let paths = repository_citations(workspace, head, log);
    if paths.is_empty() {
        return Ok(None);
    }
    let changed = annotations::candidate_changed_paths(workspace, base, head)?;
    if !paths.is_disjoint(&changed) {
        return Ok(None);
    }
    let response = host
        .run_private_vcs_operation(
            operations::CI_COMMIT_JOBS,
            json!({"commit":tip,"workspace_path":workspace,"latest_push_branch":base_branch}),
        )
        .map_err(|error| format!("the base re-check on {tip} could not be read: {error}"))?;
    let base_jobs = response["jobs"]
        .as_array()
        .ok_or("the base re-check carried no job list")?;
    let same = base_jobs
        .iter()
        .filter(|base_job| jobs::same_job(base_job, job))
        .collect::<Vec<_>>();
    let completed = job["completed_at"]
        .as_str()
        .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok());
    if let [base_job] = same.as_slice()
        && base_job["status"] == "completed"
        && base_job["conclusion"] == "success"
        && let Some(completed) = completed
        && base_job["started_at"]
            .as_str()
            .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
            .is_some_and(|started| started >= completed)
    {
        return Ok(None);
    }
    Ok(Some(format!(
        "base-suspect: its cited repository files ({}) are outside the candidate's changed paths; \
         waiting for the next push run of '{}' on base {tip} to re-check this base-wide failure \
         after the candidate failure; an earlier passing base run does not authorize repair",
        paths.into_iter().take(20).collect::<Vec<_>>().join(", "),
        jobs::job_label(job),
    )))
}

/// Surface the base-wide suspicion once per owning task and exact failure,
/// independent of poll count or PR head. This spends no repair reservation.
pub(super) fn record_base_suspect<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[String],
    evidence: &Value,
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
        tracing::warn!(task, %evidence, "base-suspect: waiting for a fresh base check; no candidate repair");
    }
    Ok(())
}
