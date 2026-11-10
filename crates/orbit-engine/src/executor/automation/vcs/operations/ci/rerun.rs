//! Authenticate the complete set affected by a workflow-wide failed-jobs retry.
use super::super::super::completion_deadline::Deadline;
use super::super::{
    DEFAULT_TIMEOUT_MS,
    input::{number, required_string, same, text},
    prerequisite::{self, api, complete_list, unreadable},
    recovery_log,
};
use orbit_common::OrbitError;
use orbit_types::workflow::{CiRerunPlan, CiRerunRoot};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path};

/// A plan is evidence, not authority: the host must reserve it before sending.
pub(in crate::executor::automation::vcs::operations) fn ci_rerun_plan(
    input: &Value,
) -> Result<Value, OrbitError> {
    let workspace = Path::new(required_string(input, "workspace_path")?);
    let deadline = Deadline::from_input(input)?;
    let pr = required_string(input, "pr")?;
    let head = required_string(input, "head_sha")?;
    let branch = required_string(input, "head_branch")?;
    let base = required_string(input, "base_branch")?;
    let accepted = required_string(input, "accepted_base")?;
    if !super::super::input::valid_expected_remote_sha(Some(accepted)) {
        return Err(unreadable("accepted retry base revision"));
    }
    let run_id = number(input, "run_id", unreadable)?;
    let roots: Vec<CiRerunRoot> =
        serde_json::from_value(input["roots"].clone()).map_err(|_| unreadable("retry roots"))?;
    if roots.is_empty()
        || roots.len() > 100
        || roots.iter().any(|root| {
            !matches!(
                root.classification.as_str(),
                "flaky" | "transient" | "unattributed"
            ) || root.signature.is_empty()
        })
    {
        return Err(unreadable("ineligible retry roots"));
    }
    let snapshot = prerequisite::snapshot_once(deadline, workspace, pr, head, branch, base)?;
    let runs = complete_list(&snapshot.runs, "workflow_runs")?;
    let listed = runs
        .iter()
        .find(|run| run["id"].as_u64() == Some(run_id))
        .ok_or_else(|| unreadable("retry run"))?;
    let workflow_id = number(listed, "workflow_id", unreadable)?;
    if runs.iter().any(|run| {
        run["workflow_id"].as_u64() == Some(workflow_id)
            && run["id"].as_u64().is_none_or(|id| id > run_id)
    }) {
        return Err(unreadable("superseded retry run"));
    }
    let run = prerequisite::checked_run(deadline, workspace, &snapshot, listed)?;
    let conclusion_only = roots.iter().all(|root| {
        matches!(
            root.signature.as_str(),
            "concluded cancelled" | "concluded timed_out"
        )
    });
    let run_conclusion = run["conclusion"].as_str();
    let run_allowed = if conclusion_only {
        matches!(run_conclusion, Some("failure" | "cancelled" | "timed_out"))
    } else {
        run_conclusion == Some("failure")
    };
    if run["status"] != "completed" || !run_allowed {
        return Err(unreadable("retry requires a completed failed attempt"));
    }
    let attempt = number(&run, "run_attempt", unreadable)?;
    let listing = api(
        deadline,
        workspace,
        &format!(
            "repos/{{owner}}/{{repo}}/actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100"
        ),
    )?;
    let jobs = complete_list(&listing, "jobs")?;
    let checks = complete_list(&snapshot.checks, "check_runs")?;
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut failed = Vec::new();
    for job in jobs {
        let job_id = number(job, "id", unreadable)?;
        if !ids.insert(job_id) || !names.insert(text(job, "name", unreadable)?) {
            return Err(unreadable("ambiguous retry job identity"));
        }
        let stopped_root = conclusion_authenticated(&roots, job_id, job);
        same(&job["run_id"], &json!(run_id), "retry job run", unreadable)?;
        same(
            &job["run_attempt"],
            &json!(attempt),
            "retry job attempt",
            unreadable,
        )?;
        same(&job["head_sha"], &json!(head), "retry job head", unreadable)?;
        if job["status"] != "completed"
            || !(matches!(
                job["conclusion"].as_str(),
                Some("success" | "skipped" | "failure")
            ) || stopped_root)
        {
            return Err(unreadable("pending or unknown retry job outcome"));
        }
        let steps = job["steps"]
            .as_array()
            .ok_or_else(|| unreadable("retry job steps"))?;
        let mut step_numbers = BTreeSet::new();
        for step in steps {
            let step_conclusion = step["conclusion"].as_str();
            let step_allowed = matches!(step_conclusion, Some("success" | "skipped" | "failure"))
                || (stopped_root && matches!(step_conclusion, Some("cancelled" | "timed_out")));
            if !step_numbers.insert(number(step, "number", unreadable)?)
                || step["status"] != "completed"
                || !step_allowed
            {
                return Err(unreadable("incomplete retry step outcomes"));
            }
        }
        if job["conclusion"] != "failure" && !stopped_root {
            continue;
        }
        if job["conclusion"] == "failure"
            && !steps.iter().any(|step| step["conclusion"] == "failure")
        {
            return Err(unreadable("failure without a failed step"));
        }
        let suffix = job["check_run_url"]
            .as_str()
            .and_then(|url| {
                url.strip_prefix(&format!(
                    "https://api.github.com/repos/{}/check-runs/",
                    snapshot.repository
                ))
            })
            .and_then(|id| id.parse::<u64>().ok())
            .ok_or_else(|| unreadable("retry check identity"))?;
        let check = checks
            .iter()
            .find(|check| {
                check["id"].as_u64() == Some(suffix)
                    && check["name"] == job["name"]
                    && check["check_suite"]["id"] == run["check_suite_id"]
            })
            .ok_or_else(|| unreadable("retry job check binding"))?;
        let check_conclusion_ok = if stopped_root {
            match job["conclusion"].as_str() {
                Some("cancelled") => {
                    matches!(check["conclusion"].as_str(), Some("cancelled" | "failure"))
                }
                Some("timed_out") => {
                    matches!(check["conclusion"].as_str(), Some("timed_out" | "failure"))
                }
                _ => false,
            }
        } else {
            check["conclusion"] == "failure"
        };
        if check["app"]["slug"] != "github-actions"
            || check["status"] != "completed"
            || !check_conclusion_ok
            || snapshot.required.iter().any(|gate| {
                gate.context == job["name"]
                    && gate
                        .app_id
                        .is_some_and(|app| check["app"]["id"].as_u64() != Some(app))
            })
        {
            return Err(unreadable("retry check policy or integration"));
        }
        failed.push(job.clone());
    }
    if failed.is_empty()
        || roots
            .iter()
            .map(|root| root.job_id)
            .collect::<BTreeSet<_>>()
            .len()
            != roots.len()
    {
        return Err(unreadable("empty or ambiguous retry selection"));
    }
    let (path, blob, source) = prerequisite::workflow(deadline, workspace, &run)?;
    // Trust is rooted in the accepted base, rather than a candidate-authored workflow.
    let trusted = api(
        deadline,
        workspace,
        &format!("repos/{{owner}}/{{repo}}/contents/{path}?ref={accepted}"),
    )?;
    same(
        &trusted["type"],
        &json!("file"),
        "accepted workflow file",
        unreadable,
    )?;
    same(
        &trusted["path"],
        &json!(path),
        "accepted workflow path",
        unreadable,
    )?;
    same(
        &trusted["sha"],
        &json!(blob),
        "accepted workflow source",
        unreadable,
    )?;
    let mut root_ids = BTreeSet::new();
    let mut logs = Vec::new();
    for root in &roots {
        let job = failed
            .iter()
            .find(|job| job["id"].as_u64() == Some(root.job_id))
            .ok_or_else(|| unreadable("retry root absent from failed set"))?;
        let expected_check = format!(
            "{} / {}",
            text(&run, "name", unreadable)?,
            text(job, "name", unreadable)?
        );
        if root.check != expected_check {
            return Err(unreadable("retry root check identity"));
        }
        if conclusion_authenticated(&roots, root.job_id, job) {
            // The provider conclusion is the evidence. The log is often gone.
            root_ids.insert(root.job_id);
            logs.push(json!({"job_id":root.job_id,"authenticated":"conclusion"}));
            continue;
        }
        if handoff_step_authenticated(root, job) {
            // The provider's failed-step name is the evidence; a handoff
            // job's log is often truncated or has no failing command.
            root_ids.insert(root.job_id);
            logs.push(json!({"job_id":root.job_id,"authenticated":"failed_step"}));
            continue;
        }
        let log = recovery_log::complete_job_log(deadline, workspace, run_id, job, &source)?;
        let failed_steps = job["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|step| step["conclusion"] == "failure")
            .collect::<Vec<_>>();
        let [step] = failed_steps.as_slice() else {
            return Err(unreadable("ambiguous root failure"));
        };
        let signature = super::super::super::super::ci::log_signature::error_signature(
            &log.text,
            text(step, "name", unreadable)?,
        );
        if signature.step_fallback || signature.text != root.signature {
            return Err(unreadable("retry root diagnostic changed"));
        }
        root_ids.insert(root.job_id);
        logs.push(json!({"job_id":root.job_id,"log":log.text,"proof":log.proof}));
    }
    let mut dependencies = Vec::new();
    for job in failed
        .iter()
        .filter(|job| !root_ids.contains(&job["id"].as_u64().unwrap_or_default()))
    {
        let log = recovery_log::complete_job_log(deadline, workspace, run_id, job, &source)?;
        // Regions may omit another record or an independent waiter diagnostic.
        // Only a complete command/failed-step log can prove record uniqueness.
        if log.proof["kind"] == "selected_failure_regions" {
            return Err(unreadable(
                "dependency authentication needs a complete producer command, not selected failure regions",
            ));
        }
        dependencies.push(
            super::super::dependency_record::authenticate_dependency_record(
                deadline, workspace, &snapshot, &run, job, &failed, accepted, &path, &source,
                &log.text,
            )?,
        );
        logs.push(json!({"job_id":job["id"],"log":log.text,"proof":log.proof}));
    }
    let mut covered = root_ids.clone();
    for _ in 0..dependencies.len() {
        let before = covered.len();
        for dependency in &dependencies {
            if dependency["dependency_ids"].as_array().is_some_and(|ids| {
                ids.iter()
                    .all(|id| id.as_u64().is_some_and(|id| covered.contains(&id)))
            }) {
                covered.insert(number(dependency, "job_id", unreadable)?);
            }
        }
        if covered.len() == before {
            break;
        }
    }
    if failed
        .iter()
        .any(|job| job["id"].as_u64().is_none_or(|id| !covered.contains(&id)))
    {
        return Err(unreadable("unproven or cyclic retry dependency closure"));
    }
    let plan = CiRerunPlan {
        schema_version: 1,
        repository: snapshot.repository,
        repository_id: snapshot.repository_id,
        pr_number: pr.into(),
        head_sha: head.into(),
        head_branch: branch.into(),
        base_branch: base.into(),
        accepted_base: accepted.into(),
        run_id,
        run_attempt: attempt,
        workflow_id,
        roots,
        failed_jobs: failed,
        authentication: json!({"run":run,"jobs":listing,"required":snapshot.required,"policy":snapshot.policy,"checks":snapshot.checks,"statuses":snapshot.statuses,"workflow_path":path,"workflow_blob":blob,"dependencies":dependencies,"logs":logs}),
    };
    deadline.remaining()?;
    if serde_json::to_vec(&plan)
        .map_err(|_| unreadable("CI retry serialization"))?
        .len()
        > 512 * 1024
    {
        return Err(unreadable("CI retry authorization exceeds its bound"));
    }
    Ok(json!({"plan":plan}))
}

/// A transient root authenticated by the provider conclusion, not a log.
fn conclusion_authenticated(roots: &[CiRerunRoot], job_id: u64, job: &Value) -> bool {
    let Some(conclusion) = job["conclusion"].as_str() else {
        return false;
    };
    matches!(conclusion, "cancelled" | "timed_out")
        && roots.iter().any(|root| {
            root.job_id == job_id
                && root.classification == "transient"
                && root.signature == format!("concluded {conclusion}")
        })
}

/// A transient root whose signature names the dependency-handoff step the
/// provider reports as the failed job's one failed step. Which step names
/// are handoffs is the caller's configured classification; this binds the
/// root to the provider's own step listing.
fn handoff_step_authenticated(root: &CiRerunRoot, job: &Value) -> bool {
    let Some(step) = root
        .signature
        .strip_prefix(orbit_types::workflow::CI_HANDOFF_STEP_SIGNATURE_PREFIX)
    else {
        return false;
    };
    let failed_steps = job["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|candidate| candidate["conclusion"] == "failure")
        .collect::<Vec<_>>();
    root.classification == "transient"
        && job["conclusion"] == "failure"
        && matches!(failed_steps.as_slice(), [only] if only["name"].as_str() == Some(step))
}

/// Rerun one workflow run's failed jobs. Single-shot: the landing reruns a
/// failure at most once, and a retried request could queue a second rerun.
pub(in crate::executor::automation::vcs::operations) fn ci_rerun_failed(
    input: &Value,
) -> Result<Value, OrbitError> {
    let run_id = required_string(input, "run_id")?;
    let workspace_path = required_string(input, "workspace_path")?;
    if !run_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(OrbitError::InvalidInput(format!(
            "invalid private automation VCS workflow run id '{run_id}'"
        )));
    }
    if let Some(expected) = input.get("plan") {
        let plan: orbit_types::workflow::CiRerunPlan = serde_json::from_value(expected.clone())
            .map_err(|_| super::super::prerequisite::unreadable("CI retry reservation"))?;
        if plan.run_id.to_string() != run_id {
            return Err(super::super::prerequisite::unreadable(
                "CI retry action run",
            ));
        }
        let refreshed = ci_rerun_plan(
            &json!({"workspace_path":workspace_path,"pr":plan.pr_number,"head_sha":plan.head_sha,"head_branch":plan.head_branch,"base_branch":plan.base_branch,"accepted_base":plan.accepted_base,"run_id":plan.run_id,"roots":plan.roots,"deadline_ms":input["deadline_ms"]}),
        )?;
        let refreshed: orbit_types::workflow::CiRerunPlan =
            serde_json::from_value(refreshed["plan"].clone())
                .map_err(|_| super::super::prerequisite::unreadable("refreshed CI retry plan"))?;
        if refreshed != plan {
            return Err(super::super::prerequisite::unreadable(
                "CI retry authorization changed before send",
            ));
        }
    }
    let deadline = input
        .get("prerequisite")
        .or_else(|| input.get("plan"))
        .map(|_| super::super::super::completion_deadline::Deadline::from_input(input))
        .transpose()?;
    if let Some(deadline) = deadline {
        deadline.remaining()?;
    }
    if let Some(evidence) = input.get("prerequisite") {
        let expected: orbit_types::workflow::PrInfrastructureEvidence =
            serde_json::from_value(evidence.clone())
                .map_err(|_| super::super::prerequisite::unreadable("rerun reservation"))?;
        if expected.run_id.to_string() != run_id {
            return Err(super::super::prerequisite::unreadable("rerun run identity"));
        }
        let observed = super::super::prerequisite::observe(
            &json!({"workspace_path":workspace_path,"evidence":expected,"deadline_ms":input["deadline_ms"]}),
        )?;
        if observed["attempt"].as_u64() != Some(u64::from(expected.run_attempt))
            || observed["status"] != "completed"
            || observed["conclusion"] != "failure"
        {
            return Err(super::super::prerequisite::unreadable(
                "attempt advanced before mutation",
            ));
        }
        let refreshed = super::super::prerequisite::evidence(
            &json!({"workspace_path":workspace_path,"pr":expected.pr_number,"head_sha":expected.head_sha,"head_branch":expected.head_branch,"base_branch":expected.base_branch,"deadline_ms":input["deadline_ms"]}),
        )?;
        let refreshed: orbit_types::workflow::PrInfrastructureEvidence =
            serde_json::from_value(refreshed["evidence"].clone())
                .map_err(|_| super::super::prerequisite::unreadable("refreshed prerequisite"))?;
        if refreshed != expected {
            return Err(super::super::prerequisite::unreadable(
                "evidence changed before mutation",
            ));
        }
    }
    let timeout_ms = match deadline {
        Some(deadline) => deadline.remaining()?,
        None => DEFAULT_TIMEOUT_MS,
    };
    let result = super::super::process::run_vcs_process(
        "gh",
        vec![
            "run".to_string(),
            "rerun".to_string(),
            run_id.to_string(),
            "--failed".to_string(),
        ],
        Some(Path::new(workspace_path)),
        timeout_ms,
    )?;
    if input.get("prerequisite").is_some()
        && !result.success
        && !result.timed_out
        && ["(HTTP 401)", "(HTTP 403)"]
            .iter()
            .any(|status| result.stderr.trim_end().ends_with(status))
    {
        return Ok(json!({"refusal":"credential_or_permission","diagnostic":result.stderr}));
    }
    let result = super::super::process::succeeded(result, "workflow rerun")?;
    Ok(json!({ "stdout": result.stdout, "stderr": result.stderr }))
}
