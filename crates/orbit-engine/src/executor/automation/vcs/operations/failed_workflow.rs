//! Forge evidence for workflows that failed before creating any jobs.
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::{
    PrFailedWorkflowRun, PrInfrastructureEvidence, PrInfrastructureFailure,
};
use serde_json::{Value, json};

use super::super::completion_deadline::Deadline;
use super::input::{number, same, text};
use super::prerequisite::{self, Snapshot, api, complete_list, unreadable};

fn workflow_path(run: &Value) -> Result<&str, OrbitError> {
    let path = text(run, "path", unreadable)?;
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 3
        || parts[0] != ".github"
        || parts[1] != "workflows"
        || parts[2].is_empty()
        || parts[2].contains("..")
        || !(path.ends_with(".yml") || path.ends_with(".yaml"))
    {
        return Err(unreadable("failed workflow path"));
    }
    Ok(path)
}

/// A failed workflow can have no PR linkage or check runs at all. Authenticate
/// its repository, branch, head, workflow and latest attempt directly instead.
fn verified_run(
    deadline: Deadline,
    workspace: &Path,
    snapshot: &Snapshot,
    listed: &Value,
) -> Result<Value, OrbitError> {
    let id = number(listed, "id", unreadable)?;
    let run = api(
        deadline,
        workspace,
        &format!("repos/{{owner}}/{{repo}}/actions/runs/{id}"),
    )?;
    for key in [
        "id",
        "workflow_id",
        "run_attempt",
        "path",
        "status",
        "conclusion",
    ] {
        same(
            &run[key],
            &listed[key],
            "failed workflow run changed",
            unreadable,
        )?;
    }
    for key in ["repository", "head_repository"] {
        same(
            &run[key]["id"],
            &json!(snapshot.repository_id),
            "failed workflow repository",
            unreadable,
        )?;
    }
    same(
        &run["head_sha"],
        &snapshot.pr["head"]["sha"],
        "failed workflow head",
        unreadable,
    )?;
    same(
        &run["head_branch"],
        &snapshot.pr["head"]["ref"],
        "failed workflow branch",
        unreadable,
    )?;
    if !matches!(run["event"].as_str(), Some("pull_request" | "push")) {
        return Err(unreadable("failed workflow event"));
    }
    number(&run, "workflow_id", unreadable)?;
    number(&run, "run_attempt", unreadable)?;
    workflow_path(&run)?;
    Ok(run)
}

fn git_read(deadline: Deadline, workspace: &Path, args: &[&str]) -> Result<String, OrbitError> {
    Ok(deadline
        .execute(
            "git",
            args.iter().map(|s| (*s).into()).collect(),
            workspace,
            "failed workflow source",
        )?
        .stdout)
}

/// Only the newest execution of each workflow can diagnose an absent context.
/// A successful or unfinished run with zero jobs is never failure evidence.
pub(super) fn failed_workflow_evidence(
    deadline: Deadline,
    workspace: &Path,
    snapshot: &Snapshot,
) -> Result<Vec<PrInfrastructureEvidence>, OrbitError> {
    let checks = complete_list(&snapshot.checks, "check_runs")?;
    let statuses = complete_list(&snapshot.statuses, "statuses")?;
    let missing: Vec<_> = snapshot
        .required
        .iter()
        .filter(|gate| !prerequisite::gate_present(gate, checks, statuses))
        .collect();
    if missing.is_empty() {
        return Ok(vec![]);
    }
    let runs = complete_list(&snapshot.runs, "workflow_runs")?;
    let mut failures = Vec::new();
    for listed in runs {
        if listed["status"] != "completed"
            || !listed["conclusion"]
                .as_str()
                .is_some_and(|c| !c.is_empty() && c != "success")
            || runs.iter().any(|other| {
                other["workflow_id"] == listed["workflow_id"]
                    && other["id"].as_u64() > listed["id"].as_u64()
            })
        {
            continue;
        }
        let run_id = number(listed, "id", unreadable)?;
        let attempt = number(listed, "run_attempt", unreadable)?;
        let jobs = api(
            deadline,
            workspace,
            &format!(
                "repos/{{owner}}/{{repo}}/actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100"
            ),
        )?;
        if !complete_list(&jobs, "jobs")?.is_empty() {
            continue;
        }
        let run = verified_run(deadline, workspace, snapshot, listed)?;
        let path = workflow_path(&run)?;
        let workflow_id = number(&run, "workflow_id", unreadable)?;
        let workflow = api(
            deadline,
            workspace,
            &format!("repos/{{owner}}/{{repo}}/actions/workflows/{workflow_id}"),
        )?;
        same(
            &workflow["id"],
            &json!(workflow_id),
            "failed workflow identity",
            unreadable,
        )?;
        same(
            &workflow["path"],
            &json!(path),
            "failed workflow source path",
            unreadable,
        )?;
        let head = text(&snapshot.pr["head"], "sha", unreadable)?;
        let base = text(&snapshot.pr["base"], "sha", unreadable)?;
        let fork = git_read(deadline, workspace, &["merge-base", base, head])?;
        let changed = git_read(
            deadline,
            workspace,
            &["diff", "--name-only", "-z", fork.trim(), head, "--", path],
        )?;
        let candidate_owned = changed.split('\0').any(|file| file == path);
        // The candidate YAML itself may be malformed. Its base definition can
        // still identify the required contexts the workflow would have emitted.
        let base_revision = format!("{base}:{path}");
        let head_revision = format!("{head}:{path}");
        let (revision, source) = match git_read(deadline, workspace, &["show", &base_revision]) {
            Ok(source) => (base_revision, source),
            Err(_) => (
                head_revision.clone(),
                git_read(deadline, workspace, &["show", &head_revision])?,
            ),
        };
        if source.len() > super::MAX_WORKFLOW_SOURCE_BYTES {
            return Err(unreadable("failed workflow source bound"));
        }
        let parsed: Value =
            serde_yaml::from_str(&source).map_err(|_| unreadable("failed workflow base YAML"))?;
        let Some(source_jobs) = parsed["jobs"].as_object().filter(|jobs| jobs.len() <= 100) else {
            continue;
        };
        let names: Vec<_> = source_jobs
            .iter()
            .filter_map(|(id, job)| super::workflow_dependency::job_names(id, job).ok())
            .flatten()
            .collect();
        let suite_id = number(&run, "check_suite_id", unreadable)?;
        let suite = api(
            deadline,
            workspace,
            &format!("repos/{{owner}}/{{repo}}/check-suites/{suite_id}"),
        )?;
        same(
            &suite["id"],
            &json!(suite_id),
            "failed workflow suite",
            unreadable,
        )?;
        same(
            &suite["head_sha"],
            &json!(head),
            "failed workflow suite head",
            unreadable,
        )?;
        same(
            &suite["repository"]["id"],
            &json!(snapshot.repository_id),
            "failed workflow suite repository",
            unreadable,
        )?;
        same(
            &suite["app"]["slug"],
            &json!("github-actions"),
            "failed workflow integration",
            unreadable,
        )?;
        let app_id = number(&suite["app"], "id", unreadable)?;
        let Some(gate) = missing.iter().find(|gate| {
            gate.app_id.is_none_or(|app| app == app_id)
                && names.iter().any(|name| {
                    gate.context == *name
                        || parsed["name"]
                            .as_str()
                            .is_some_and(|workflow| gate.context == format!("{workflow} / {name}"))
                })
        }) else {
            continue;
        };
        let blob = git_read(deadline, workspace, &["rev-parse", &revision])?;
        let provider_message = deadline
            .execute(
                "gh",
                vec!["run".into(), "view".into(), run_id.to_string()],
                workspace,
                "failed workflow diagnosis",
            )
            .ok()
            .map(|result| {
                format!("{}{}", result.stdout, result.stderr)
                    .trim()
                    .to_string()
            })
            .filter(|message| !message.is_empty())
            .map(|message| {
                if message.len() > 8192 {
                    format!(
                        "{}\n[truncated]",
                        message.chars().take(2000).collect::<String>()
                    )
                } else {
                    message
                }
            });
        failures.push(PrInfrastructureEvidence {
            repository: snapshot.repository.clone(), repository_id: snapshot.repository_id,
            pr_number: snapshot.pr["number"].to_string(), head_sha: head.into(),
            head_branch: text(&snapshot.pr["head"], "ref", unreadable)?.into(), base_branch: text(&snapshot.pr["base"], "ref", unreadable)?.into(), base_sha: base.into(),
            required: snapshot.required.clone(), missing: (*gate).clone(), app_id, workflow_id,
            workflow_path: path.into(), workflow_blob: blob.trim().into(), workflow_source: source,
            failure: PrInfrastructureFailure::WorkflowRun { failed_workflow_run: PrFailedWorkflowRun { candidate_owned, provider_message } },
            run_id, run_attempt: u32::try_from(attempt).map_err(|_| unreadable("failed workflow attempt"))?, event: text(&run, "event", unreadable)?.into(),
            policy: snapshot.policy.clone(), provider: json!({"pr":snapshot.pr,"checks":snapshot.checks,"statuses":snapshot.statuses,"suite":suite,"workflow_runs":snapshot.runs}), run, jobs, log: String::new(),
        });
    }
    Ok(failures)
}

/// Observe the same reserved run after its single full-workflow rerun.
pub(super) fn observe_failed_workflow(
    deadline: Deadline,
    workspace: &Path,
    evidence: &PrInfrastructureEvidence,
) -> Result<Value, OrbitError> {
    let snapshot = prerequisite::snapshot(
        deadline,
        workspace,
        &evidence.pr_number,
        &evidence.head_sha,
        &evidence.head_branch,
        &evidence.base_branch,
    )?;
    if snapshot.repository_id != evidence.repository_id
        || snapshot.repository != evidence.repository
        || snapshot.required != evidence.required
        || snapshot.policy != evidence.policy
        || snapshot.pr["base"]["sha"] != evidence.base_sha
    {
        return Err(unreadable(
            "failed workflow reservation policy/repository/base changed",
        ));
    }
    let runs = complete_list(&snapshot.runs, "workflow_runs")?;
    if runs.iter().any(|run| {
        run["workflow_id"].as_u64() == Some(evidence.workflow_id)
            && run["id"].as_u64().is_none_or(|id| id > evidence.run_id)
    }) {
        return Err(unreadable("failed workflow reservation superseded"));
    }
    let listed = runs
        .iter()
        .find(|run| run["id"].as_u64() == Some(evidence.run_id))
        .ok_or_else(|| unreadable("failed workflow reservation disappeared"))?;
    let run = verified_run(deadline, workspace, &snapshot, listed)?;
    if run["workflow_id"].as_u64() != Some(evidence.workflow_id)
        || workflow_path(&run)? != evidence.workflow_path
        || run["event"] != evidence.event
    {
        return Err(unreadable("failed workflow reservation identity changed"));
    }
    let attempt = number(&run, "run_attempt", unreadable)?;
    let original = u64::from(evidence.run_attempt);
    if attempt < original || attempt > original + 1 {
        return Err(unreadable("failed workflow reservation attempt changed"));
    }
    let checks = complete_list(&snapshot.checks, "check_runs")?;
    let statuses = complete_list(&snapshot.statuses, "statuses")?;
    let passed = snapshot
        .required
        .iter()
        .all(|gate| prerequisite::gate_passed(gate, checks, statuses));
    if attempt > original
        && run["status"] == "completed"
        && (run["conclusion"] != "success" || !passed)
    {
        return Err(unreadable(&format!(
            "workflow {} run {} failed again or required checks remain missing; the infrastructure rerun budget is spent",
            evidence.workflow_path, evidence.run_id
        )));
    }
    if attempt == original
        && (run["status"] != "completed" || run["conclusion"] != evidence.run["conclusion"])
    {
        return Err(unreadable("original failed workflow attempt changed"));
    }
    Ok(
        json!({"attempt":attempt,"status":run["status"],"conclusion":run["conclusion"],"gates_passed":passed && attempt > original && run["status"] == "completed", "run":run}),
    )
}
