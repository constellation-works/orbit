//! A stale base result cannot attribute an untouched-file failure to a PR.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use serde_json::json;

use crate::context::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::workflow::{
    CI_BASE_SUSPECT_EVENT, CI_CHECK_RERUN_EVENT, CiBaseSuspectLegacy, CiBaseSuspectRecord,
    CiCheckRerunRecord,
};

use super::super::super::super::ci::log_signature;
use super::super::super::operations::{
    CiCommitJobEvidence, CiCommitJobsAnswer, CiJobConclusion, CiJobEvent, CiJobObservation,
    CiJobStatus,
};
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

/// A mixed failure's candidate attribution needs its own changed source.
pub(super) fn cites_changed_candidate(
    workspace: &Path,
    head: &str,
    base: &str,
    log: &str,
) -> Result<bool, OrbitError> {
    let cited = repository_citations(workspace, head, log).map_err(OrbitError::Execution)?;
    let changed = git_output_paths(workspace, &["diff", "--name-only", "-z", base, head])?;
    Ok(cited.iter().any(|path| changed.contains(path)))
}

/// Lossless projection of a numeric job observation into a base-suspect
/// legacy field: missing, null and malformed observations keep their shape.
pub(super) fn base_suspect_legacy_number(
    observation: &CiJobObservation<u64>,
) -> CiBaseSuspectLegacy<u64> {
    match observation {
        CiJobObservation::Missing => CiBaseSuspectLegacy::Missing,
        CiJobObservation::Null => CiBaseSuspectLegacy::Null,
        CiJobObservation::Known { value, .. } => CiBaseSuspectLegacy::Known(*value),
        CiJobObservation::Malformed(wire) => CiBaseSuspectLegacy::Unreadable(wire.clone()),
    }
}

/// Lossless projection of a text job observation; see
/// [`base_suspect_legacy_number`].
pub(super) fn base_suspect_legacy_text(
    observation: &CiJobObservation<String>,
) -> CiBaseSuspectLegacy<String> {
    match observation {
        CiJobObservation::Missing => CiBaseSuspectLegacy::Missing,
        CiJobObservation::Null => CiBaseSuspectLegacy::Null,
        CiJobObservation::Known { value, .. } => CiBaseSuspectLegacy::Known(value.clone()),
        CiJobObservation::Malformed(wire) => CiBaseSuspectLegacy::Unreadable(wire.clone()),
    }
}

/// Lossless projection of a job-event observation; the recorded token is
/// the provider's original text.
pub(super) fn base_suspect_legacy_event(
    observation: &CiJobObservation<CiJobEvent>,
) -> CiBaseSuspectLegacy<String> {
    match observation {
        CiJobObservation::Missing => CiBaseSuspectLegacy::Missing,
        CiJobObservation::Null => CiBaseSuspectLegacy::Null,
        CiJobObservation::Known { wire, .. } => match wire.as_str() {
            Some(token) => CiBaseSuspectLegacy::Known(token.to_string()),
            None => CiBaseSuspectLegacy::Unreadable(wire.clone()),
        },
        CiJobObservation::Malformed(wire) => CiBaseSuspectLegacy::Unreadable(wire.clone()),
    }
}

/// Independently guard the repair evidence path, including job-level recovery.
#[allow(
    clippy::too_many_arguments,
    reason = "suspicion checks the job log, signature and pull number against the candidate diff in one pass"
)]
pub(super) fn watcher_suspicion<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    tasks: &[String],
    head: &str,
    base_branch: Option<&str>,
    job: &CiCommitJobEvidence,
    log: &str,
    signature: &str,
    pr_number: Option<u64>,
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
    let evidence = untouched_base_evidence(
        host,
        workspace,
        head,
        base.trim(),
        tip.trim(),
        branch,
        job,
        paths,
    )?;
    match evidence {
        UntouchedBaseEvidence::Waiting(reason) => Ok(Some(reason)),
        UntouchedBaseEvidence::PassingBaseRun
            if !has_matching_candidate_rerun(
                host,
                tasks,
                head,
                job,
                &jobs::job_label(job),
                signature,
                pr_number,
            ) =>
        {
            let reason = format!(
                "base-suspect: '{}' has a successful same-check base run, but head {head} \
                 has no unchanged-head pull_request rerun with the same failure signature; \
                 a base pass alone does not authorize repair",
                jobs::job_label(job),
            );
            // Retain this first failure so a later unchanged-head rerun with
            // the same signature can be matched by
            // `has_matching_candidate_rerun`, even when no landing or
            // auto-fix pass runs to record it (for example, completion's
            // final recovery with auto-fix disabled).
            let record = CiBaseSuspectRecord {
                base: Some(tip.trim().to_string()),
                head: Some(head.to_string()),
                check: Some(jobs::job_label(job)),
                signature: Some(signature.to_string()),
                run_id: base_suspect_legacy_number(&job.run_id),
                job_id: base_suspect_legacy_number(&job.job_id),
                run_attempt: base_suspect_legacy_number(&job.run_attempt),
                pr_number,
                event: base_suspect_legacy_event(&job.event),
                started_at: base_suspect_legacy_text(&job.started_at),
                completed_at: base_suspect_legacy_text(&job.completed_at),
                reason: Some(reason.clone()),
                legacy_pull_requests: Vec::new(),
            };
            // A refused write must keep the hold: an error here would reach
            // completion's final recovery as an unjudged failure. The next
            // observation of this failure records it again.
            if let Err(error) = record_base_suspect(host, tasks, &record, None) {
                tracing::warn!(%error, "base-suspect: the first failure could not be recorded; holding it and recording it on a later observation");
            }
            Ok(Some(reason))
        }
        UntouchedBaseEvidence::Unrelated | UntouchedBaseEvidence::PassingBaseRun => Ok(None),
    }
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
#[allow(
    clippy::too_many_arguments,
    reason = "an untouched failure compares head, base and tip with the job log before any rerun"
)]
pub(super) fn stale_untouched_failure<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    head: &str,
    base: &str,
    tip: &str,
    base_branch: &str,
    job: &CiCommitJobEvidence,
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
pub(super) fn started_after_completion(
    job: &CiCommitJobEvidence,
    other: &CiCommitJobEvidence,
) -> bool {
    let completed = job.completed_time;
    let started = other.started_time;
    match (completed, started) {
        (Some(completed), Some(started)) => started >= completed,
        _ => false,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "base evidence takes the cited paths plus head, base, tip and the job that failed"
)]
fn untouched_base_evidence<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    head: &str,
    base: &str,
    tip: &str,
    base_branch: &str,
    job: &CiCommitJobEvidence,
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
    let base_jobs = CiCommitJobsAnswer::decode_response(response)
        .ok_or("the base re-check carried no job list")?
        .jobs;
    if base_jobs.iter().any(|base_job| {
        jobs::same_job(base_job, job)
            && base_job.run_status.value() == Some(&CiJobStatus::Completed)
            && base_job.status.value() == Some(&CiJobStatus::Completed)
            && base_job.conclusion.value() == Some(&CiJobConclusion::Success)
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

/// Whether `job` is authenticated provider evidence of a `pull_request`
/// run for `pr_number` on `head`: the event, the PR association and the
/// tested head must all agree. Metadata that names no PR, or a different
/// one, is not a PR rerun.
pub(super) fn is_verified_pull_request_job(
    job: &CiCommitJobEvidence,
    pr_number: Option<u64>,
    head: &str,
) -> bool {
    if job.event.value() != Some(&CiJobEvent::PullRequest) {
        return false;
    }
    if let Some(sha) = job.head_sha.text()
        && sha != head
    {
        return false;
    }
    let Some(pr_number) = pr_number else {
        return false;
    };
    job.association.numbers.contains(&pr_number)
}

/// Whether task history records an earlier failed attempt with this same
/// signature on the same head, and the current job is a verified rerun of
/// that same provider run: a `pull_request` execution for the same PR and
/// head, with the same run identity and a later attempt, starting after
/// the earlier attempt completed. A suspicion record alone proves no
/// rerun was requested, and a different run — a later push or
/// workflow_dispatch run on the same head, or a rerun of another run —
/// is not a rerun of the recorded failure. The first failure is retained
/// before the rerun is requested, so a resumed landing or auto-fix can
/// recognize the same evidence as an in-process rerun; a durable
/// `ci_check_rerun` note written before PR identity was recorded still
/// qualifies when the current job itself carries that identity.
pub(super) fn has_matching_candidate_rerun<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[String],
    head: &str,
    job: &CiCommitJobEvidence,
    check: &str,
    signature: &str,
    pr_number: Option<u64>,
) -> bool {
    if !is_verified_pull_request_job(job, pr_number, head) {
        return false;
    }
    let current_start = job.started_time;
    let Some(current_start) = current_start else {
        return false;
    };
    let (Some(current_run), Some(current_attempt)) =
        (job.run_id.number(), job.run_attempt.number())
    else {
        return false;
    };
    for task in tasks {
        let Ok(history) = host.get_task_history(task) else {
            continue;
        };
        let reservations = history
            .iter()
            .filter(|entry| entry.event == orbit_types::workflow::CI_RERUN_RESERVED_EVENT)
            .map(|entry| {
                entry
                    .note
                    .as_deref()
                    .and_then(|note| {
                        serde_json::from_str::<orbit_types::workflow::CiRerunPlan>(note).ok()
                    })
                    .filter(|plan| plan.schema_version == 1)
            })
            .collect::<Option<Vec<_>>>();
        let Some(reservations) = reservations else {
            return false;
        };
        let same_head = reservations
            .iter()
            .filter(|plan| plan.head_sha == head && plan.pr_number.parse::<u64>().ok() == pr_number)
            .collect::<Vec<_>>();
        if same_head.windows(2).any(|pair| pair[0] != pair[1]) {
            return false;
        }
        for plan in same_head {
            if job.repository_id.number() != Some(plan.repository_id)
                || job.repository.text() != Some(plan.repository.as_str())
                || job.head_branch.text() != Some(plan.head_branch.as_str())
                || job.head_sha.text() != Some(head)
            {
                return false;
            }
            if plan.head_sha != head
                || plan.pr_number.parse::<u64>().ok() != pr_number
                || plan.run_id != current_run
                || plan.run_attempt.checked_add(1) != Some(current_attempt)
                || job.workflow_id.number() != Some(plan.workflow_id)
            {
                continue;
            }
            for root in &plan.roots {
                let prior = plan
                    .failed_jobs
                    .iter()
                    .find(|prior| prior["id"].as_u64() == Some(root.job_id));
                if root.classification == "flaky"
                    && root.check == check
                    && root.signature == signature
                    && prior.is_some_and(|prior| {
                        job.name.matches_legacy(&prior["name"])
                            && prior["completed_at"]
                                .as_str()
                                .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
                                .is_some_and(|completed| current_start >= completed)
                    })
                {
                    return true;
                }
            }
        }
        for entry in &history {
            let prior = match entry.event.as_str() {
                CI_CHECK_RERUN_EVENT => {
                    let Some(receipt) =
                        CiCheckRerunRecord::from_history_note(entry.note.as_deref())
                    else {
                        continue;
                    };
                    // A receipt that names a different PR cannot be this PR's rerun.
                    if names_other_pull_request(receipt.pull_request_numbers(), pr_number)
                        || !receipt.reran_pull_request()
                    {
                        continue;
                    }
                    let Some(rerun) = receipt.jobs.iter().find(|rerun| {
                        rerun.check.as_deref() == Some(check)
                            && rerun.signature.as_deref() == Some(signature)
                    }) else {
                        continue;
                    };
                    PriorFailure {
                        head: receipt.head.clone(),
                        run: receipt.run_id,
                        completed: rerun.completed_time(),
                        attempt: rerun.run_attempt,
                    }
                }
                CI_BASE_SUSPECT_EVENT => {
                    let Some(suspect) =
                        CiBaseSuspectRecord::from_history_note(entry.note.as_deref())
                    else {
                        continue;
                    };
                    if names_other_pull_request(suspect.pull_request_numbers(), pr_number)
                        || suspect.check.as_deref() != Some(check)
                        || suspect.signature.as_deref() != Some(signature)
                    {
                        continue;
                    }
                    PriorFailure {
                        head: suspect.head.clone(),
                        run: suspect.run_id.value().copied(),
                        completed: suspect.completed_time(),
                        attempt: suspect.run_attempt.value().copied(),
                    }
                }
                _ => continue,
            };
            let (Some(prior_head), Some(prior_run)) = (prior.head.as_deref(), prior.run) else {
                continue;
            };
            if prior_head != head || prior_run != current_run {
                continue;
            }
            let Some(completed) = prior.completed else {
                continue;
            };
            if current_start < completed {
                continue;
            }
            // A rerun keeps the run identity and advances its attempt.
            // The numeric job id may be reassigned for the new attempt;
            // the workflow/job identity is already bound by `check`.
            let later_attempt = match prior.attempt {
                Some(prior_attempt) => current_attempt > prior_attempt,
                None => current_attempt > 1,
            };
            if later_attempt {
                return true;
            }
        }
    }
    false
}

/// The earlier failed attempt a rerun receipt or base-suspect note records.
struct PriorFailure {
    head: Option<String>,
    run: Option<u64>,
    completed: Option<chrono::DateTime<chrono::FixedOffset>>,
    attempt: Option<u64>,
}

/// Whether a note names PR numbers and none of them is `pr_number`.
fn names_other_pull_request(noted: impl Iterator<Item = u64>, pr_number: Option<u64>) -> bool {
    let noted = noted.collect::<Vec<_>>();
    !noted.is_empty() && pr_number.is_some_and(|pr_number| !noted.contains(&pr_number))
}

/// Surface the base-wide suspicion once per owning task and exact failure,
/// keyed by head, check and signature, even when the base advances.
/// This spends no repair reservation.
pub(super) fn record_base_suspect<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[String],
    evidence: &CiBaseSuspectRecord,
    handoff_id: Option<&str>,
) -> Result<(), OrbitError> {
    let note = evidence
        .to_history_note()
        .map_err(|error| OrbitError::Execution(error.to_string()))?;
    for task in tasks {
        if host
            .get_task_history(task)?
            .iter()
            .filter(|entry| entry.event == CI_BASE_SUSPECT_EVENT)
            .filter_map(|entry| CiBaseSuspectRecord::from_history_note(entry.note.as_deref()))
            .any(|prior| evidence.same_dedupe_identity(&prior))
        {
            continue;
        }
        if let Some(handoff_id) = handoff_id {
            host.record_handoff_landing_history(handoff_id, CI_BASE_SUSPECT_EVENT, &note)?;
        } else {
            host.apply_task_automation_update(
                task,
                TaskAutomationUpdate {
                    append_history: vec![
                        orbit_types::task::TaskHistoryEntry::new(
                            chrono::Utc::now(),
                            "system",
                            CI_BASE_SUSPECT_EVENT,
                        )
                        .with_note(Some(note.clone())),
                    ],
                    ..TaskAutomationUpdate::default()
                },
            )?;
        }
        tracing::warn!(task, evidence = %note, "base-suspect: waiting for conclusive same-check base and pull-request rerun evidence; no candidate repair");
    }
    Ok(())
}
