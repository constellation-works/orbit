use std::path::Path;

use orbit_common::{OrbitError, PrivateVcsFailure, PrivateVcsFailureClass, ProviderReadThrottle};
use orbit_types::workflow::PrInfrastructureEvidence;
use serde_json::{Value, json};

use super::super::super::completion_deadline::Deadline;
use super::super::CI_PREREQUISITE_EVIDENCE;
use super::super::ci::ci_job_log;
use super::super::input::{number, required_string, same, text};
use super::super::workflow_dependency::dependency;
use super::snapshot::{PullRequestSelector, checked_run, poll_interval_ms, snapshot};
use super::support::{api, complete_list, unreadable};
use super::workflow::{gate_present, workflow};

/// Diagnose only an absent protected context with a static failed dependency.
/// A rate-limited read anywhere in the pass, after the snapshot included,
/// waits for the provider's reset within the deadline and reads the evidence
/// again; nothing here mutates.
pub(in super::super) fn evidence(input: &Value) -> Result<Value, OrbitError> {
    let workspace = Path::new(required_string(input, "workspace_path")?);
    let deadline = Deadline::from_input(input)?;
    super::super::read_reset::read_through_throttles(deadline, workspace, |pass| {
        evidence_once(input, pass)
    })
}

/// [ORB-25918] A prerequisite log whose fallback read the provider throttled.
/// The throttle stays on the error as a private VCS failure, so the pass
/// waits for the reset inside its deadline and reads the log again. When no
/// wait fits, the provider's diagnostic rides the held stop.
fn throttled_prerequisite_log(diagnostic: &str, throttle: &ProviderReadThrottle) -> OrbitError {
    OrbitError::PrivateVcs(Box::new(PrivateVcsFailure {
        operation: CI_PREREQUISITE_EVIDENCE.to_string(),
        class: PrivateVcsFailureClass::Other,
        message: format!("workflow prerequisite log could not be read: {diagnostic}"),
        read_throttle: Some(throttle.clone()),
    }))
}

fn evidence_once(
    input: &Value,
    pass: &super::super::read_reset::SnapshotWaits,
) -> Result<Value, OrbitError> {
    let workspace = Path::new(required_string(input, "workspace_path")?);
    let pr = required_string(input, "pr")?;
    let head = required_string(input, "head_sha")?;
    let branch = required_string(input, "head_branch")?;
    let base = required_string(input, "base_branch")?;
    let deadline = Deadline::from_input(input)?;
    let snapshot = snapshot(
        deadline,
        workspace,
        PullRequestSelector {
            pr,
            head,
            branch,
            base,
        },
        poll_interval_ms(input),
        pass,
    )?;
    let checks = complete_list(&snapshot.checks, "check_runs")?;
    let statuses = complete_list(&snapshot.statuses, "statuses")?;
    let missing = snapshot
        .required
        .iter()
        .filter(|gate| !gate_present(gate, checks, statuses))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(json!({"evidence":null}));
    }
    // Validation failures can have neither check runs nor PR linkage on the
    // workflow run. Authenticate those runs directly before the stricter
    // prerequisite-job path, which requires a linked PR and an actual job.
    if checks.is_empty()
        || complete_list(&snapshot.runs, "workflow_runs")?
            .iter()
            .any(|run| run["pull_requests"].as_array().is_some_and(Vec::is_empty))
    {
        let failures = super::super::failed_workflow::failed_workflow_evidence(
            deadline, workspace, &snapshot,
        )?;
        if let Some(evidence) = failures.into_iter().find(|evidence| {
            evidence
                .failed_workflow_run()
                .is_some_and(|failure| !failure.candidate_owned)
        }) {
            return Ok(json!({"evidence":evidence}));
        }
    }
    if statuses.iter().any(|status| {
        status["state"] != "success" || missing.iter().any(|gate| status["context"] == gate.context)
    }) {
        return Err(unreadable(
            "legacy status is not an absent workflow context",
        ));
    }
    let mut candidates = Vec::new();
    let runs = complete_list(&snapshot.runs, "workflow_runs")?;
    for listed in runs {
        if runs.iter().any(|other| {
            other["workflow_id"] == listed["workflow_id"]
                && other["id"].as_u64() > listed["id"].as_u64()
        }) {
            return Err(unreadable("superseded workflow run"));
        }
        if listed["status"] == "completed" && listed["conclusion"] == "success" {
            continue;
        }
        let run = checked_run(deadline, workspace, &snapshot, listed)?;
        if run["status"] != "completed"
            || !run["conclusion"]
                .as_str()
                .is_some_and(|conclusion| !conclusion.is_empty() && conclusion != "success")
        {
            return Err(unreadable("running, superseded or non-failed attempt"));
        }
        let run_id = number(&run, "id", unreadable)?;
        let attempt = number(&run, "run_attempt", unreadable)?;
        let jobs = api(
            deadline,
            workspace,
            &format!(
                "repos/{{owner}}/{{repo}}/actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100"
            ),
        )?;
        let job_list = complete_list(&jobs, "jobs")?;
        if job_list.is_empty() {
            let failures = super::super::failed_workflow::failed_workflow_evidence(
                deadline, workspace, &snapshot,
            )?;
            if let Some(evidence) = failures.into_iter().find(|evidence| {
                evidence.run_id == run_id
                    && evidence
                        .failed_workflow_run()
                        .is_some_and(|failure| !failure.candidate_owned)
            }) {
                return Ok(json!({"evidence":evidence}));
            }
            continue;
        }
        if run["conclusion"] != "failure" {
            return Err(unreadable("running, superseded or non-failed attempt"));
        }
        let mut job_ids = std::collections::BTreeSet::new();
        for job in job_list {
            if !job_ids.insert(number(job, "id", unreadable)?) {
                return Err(unreadable("duplicate provider job identity"));
            }
            same(&job["run_id"], &json!(run_id), "job run", unreadable)?;
            same(&job["head_sha"], &json!(head), "job head", unreadable)?;
            same(
                &job["run_attempt"],
                &json!(attempt),
                "job attempt",
                unreadable,
            )?;
            if !matches!(
                job["conclusion"].as_str(),
                Some("success" | "skipped" | "failure")
            ) {
                return Err(unreadable("ambiguous rerun job selection"));
            }
            if job["status"] != "completed" {
                return Err(unreadable("running prerequisite jobs"));
            }
        }
        let failed = job_list
            .iter()
            .filter(|job| job["conclusion"] == "failure")
            .collect::<Vec<_>>();
        let [failed] = failed.as_slice() else {
            continue;
        };
        let (path, blob, source) = workflow(deadline, workspace, &run)?;
        for gate in &missing {
            let Ok(dependency) =
                dependency(&source, &gate.context, text(failed, "name", unreadable)?)
            else {
                continue;
            };
            let failed_steps = failed["steps"]
                .as_array()
                .ok_or_else(|| unreadable("failed prerequisite steps"))?
                .iter()
                .filter(|step| step["conclusion"] == "failure")
                .collect::<Vec<_>>();
            let [failed_step] = failed_steps.as_slice() else {
                return Err(unreadable("ambiguous failed prerequisite step"));
            };
            super::super::workflow_read::api_step(
                &dependency.workflow,
                &dependency.prerequisite,
                text(failed_step, "name", unreadable)?,
            )?;
            let suite = api(
                deadline,
                workspace,
                &format!(
                    "repos/{{owner}}/{{repo}}/check-suites/{}",
                    number(&run, "check_suite_id", unreadable)?
                ),
            )?;
            same(&suite["id"], &run["check_suite_id"], "suite", unreadable)?;
            same(&suite["head_sha"], &json!(head), "suite head", unreadable)?;
            same(
                &suite["repository"]["id"],
                &json!(snapshot.repository_id),
                "suite repository",
                unreadable,
            )?;
            same(
                &suite["app"]["slug"],
                &json!("github-actions"),
                "workflow integration",
                unreadable,
            )?;
            let app_id = number(&suite["app"], "id", unreadable)?;
            if gate.app_id.is_some_and(|app| app != app_id) {
                return Err(unreadable("protected integration differs"));
            }
            super::super::workflow_dependency::certify_rerun(
                &dependency,
                job_list,
                &snapshot.required,
                app_id,
            )?;
            let job_id = number(failed, "id", unreadable)?;
            let check_url = text(failed, "check_run_url", unreadable)?;
            let check_prefix = format!(
                "https://api.github.com/repos/{}/check-runs/",
                snapshot.repository
            );
            let check_id = check_url
                .strip_prefix(&check_prefix)
                .and_then(|id| id.parse::<u64>().ok())
                .ok_or_else(|| unreadable("job check identity"))?;
            if !checks.iter().any(|check| {
                check["id"].as_u64() == Some(check_id)
                    && check["name"] == failed["name"]
                    && check["app"]["id"].as_u64() == Some(app_id)
                    && check["check_suite"]["id"] == run["check_suite_id"]
            }) {
                return Err(unreadable("prerequisite check integration"));
            }
            let log = ci_job_log(
                &json!({"workspace_path":workspace,"run_id":run_id.to_string(),"job_id":job_id.to_string(),"deadline_ms":deadline.0}),
            )?;
            if log.truncated != Some(false) || log.fallback_error.is_some() {
                return Err(match (&log.fallback_error, log.known_fallback_throttle()) {
                    (Some(diagnostic), Some(throttle)) => {
                        throttled_prerequisite_log(diagnostic, throttle)
                    }
                    _ => unreadable("truncated/unreadable prerequisite log"),
                });
            }
            let log = log
                .log
                .as_deref()
                .filter(|log| !log.is_empty())
                .ok_or_else(|| unreadable("log"))?;
            // Narrow evidence: an explicit gh HTTP 502/503/504 API failure.
            // A credential/permission diagnostic anywhere vetoes admission.
            let lower = log.to_ascii_lowercase();
            if [
                "http 401",
                "http 403",
                "bad credentials",
                "permission denied",
                "not accessible by integration",
                "authentication",
            ]
            .iter()
            .any(|token| lower.contains(token))
                || !log.lines().any(|line| {
                    line.contains("gh:")
                        && ["(HTTP 502)", "(HTTP 503)", "(HTTP 504)"]
                            .iter()
                            .any(|status| line.trim_end().ends_with(status))
                })
            {
                return Err(unreadable("no narrow transient API failure"));
            }
            candidates.push(PrInfrastructureEvidence {
                repository: snapshot.repository.clone(),
                repository_id: snapshot.repository_id,
                pr_number: pr.into(),
                head_sha: head.into(),
                head_branch: branch.into(),
                base_branch: base.into(),
                base_sha: text(&snapshot.pr["base"], "sha", unreadable)?.into(),
                required: snapshot.required.clone(),
                missing: (*gate).clone(),
                app_id,
                workflow_id: number(&run, "workflow_id", unreadable)?,
                workflow_path: path.clone(),
                workflow_blob: blob.clone(),
                workflow_source: source.clone(),
                failure: orbit_types::workflow::PrInfrastructureFailure::Prerequisite {
                    workflow_job: dependency.job,
                    prerequisite: dependency.prerequisite,
                    prerequisite_job_id: job_id,
                },
                run_id,
                run_attempt: u32::try_from(attempt).map_err(|_| unreadable("attempt overflow"))?,
                event: "pull_request".into(),
                policy: snapshot.policy.clone(),
                provider: json!({"pr":snapshot.pr,"checks":snapshot.checks,"statuses":snapshot.statuses,"suite":suite,"workflow_runs":snapshot.runs}),
                run: run.clone(),
                jobs: jobs.clone(),
                log: log.into(),
            });
            // Several missing matrix/check contexts may share this unique root.
            // The complete required set and rerun closure were certified above.
            break;
        }
    }
    if candidates.len() != 1 {
        return Err(unreadable("no unique source-bound prerequisite recovery"));
    }
    deadline.remaining()?;
    Ok(json!({"evidence":candidates.pop()}))
}
