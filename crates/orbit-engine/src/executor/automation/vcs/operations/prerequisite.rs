//! GitHub owns effective policy, workflow identities and source dependency reads.
//! The engine receives verified generic evidence; no workflow is special-cased.
use super::super::completion_deadline::Deadline;
use super::{ci::ci_job_log, input::required_string, workflow_dependency::dependency};
use orbit_common::OrbitError;
use orbit_types::workflow::{PrInfrastructureEvidence, RequiredGate};
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn unreadable(reason: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "workflow prerequisite evidence is unavailable ({reason})"
    ))
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, OrbitError> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| unreadable(key))
}
fn number(value: &Value, key: &str) -> Result<u64, OrbitError> {
    value[key]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or_else(|| unreadable(key))
}
fn same(actual: &Value, expected: Value, label: &str) -> Result<(), OrbitError> {
    if *actual != expected {
        return Err(unreadable(&format!("foreign or changed {label}")));
    }
    Ok(())
}
fn api(deadline: Deadline, workspace: &Path, endpoint: &str) -> Result<Value, OrbitError> {
    let response = deadline.execute(
        "gh",
        vec!["api".into(), endpoint.into()],
        workspace,
        &format!("workflow prerequisite read {endpoint}"),
    )?;
    serde_json::from_str(&response.stdout).map_err(|_| unreadable("invalid API JSON"))
}
fn complete_list<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>, OrbitError> {
    let list = value[key].as_array().ok_or_else(|| unreadable(key))?;
    if value["total_count"].as_u64() != Some(list.len() as u64) || list.len() > 100 {
        return Err(unreadable(&format!("truncated {key}")));
    }
    Ok(list)
}
fn encoded(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

struct Snapshot {
    repository: String,
    repository_id: u64,
    pr: Value,
    required: Vec<RequiredGate>,
    policy: Value,
    checks: Value,
    runs: Value,
}
fn snapshot(
    deadline: Deadline,
    workspace: &Path,
    pr: &str,
    head: &str,
    branch: &str,
    base: &str,
) -> Result<Snapshot, OrbitError> {
    if pr.is_empty()
        || !pr.bytes().all(|byte| byte.is_ascii_digit())
        || !super::input::valid_expected_remote_sha(Some(head))
    {
        return Err(unreadable("PR/head selector"));
    }
    let repo = api(deadline, workspace, "repos/{owner}/{repo}")?;
    let repository = text(&repo, "full_name")?.to_string();
    let repository_id = number(&repo, "id")?;
    let pull = api(
        deadline,
        workspace,
        &format!("repos/{{owner}}/{{repo}}/pulls/{pr}"),
    )?;
    same(
        &pull["number"],
        json!(pr.parse::<u64>().map_err(|_| unreadable("PR number"))?),
        "PR",
    )?;
    same(&pull["state"], json!("open"), "PR state")?;
    same(&pull["merged"], json!(false), "merged PR")?;
    for side in ["head", "base"] {
        same(
            &pull[side]["repo"]["id"],
            json!(repository_id),
            "repository",
        )?;
        same(
            &pull[side]["repo"]["full_name"],
            json!(repository),
            "repository name",
        )?;
    }
    same(&pull["head"]["sha"], json!(head), "head")?;
    same(&pull["head"]["ref"], json!(branch), "branch")?;
    same(&pull["base"]["ref"], json!(base), "base")?;
    // GraphQL supplies the effective classic rule for this exact base. The
    // active branch-rules endpoint adds organization/repository rulesets.
    let query = "query($pr:Int!,$owner:String!,$repo:String!){repository(owner:$owner,name:$repo){pullRequest(number:$pr){headRefOid baseRefName reviewDecision baseRef{branchProtectionRule{requiredStatusChecks{context app{databaseId slug}}}}}}}";
    let response = deadline.execute(
        "gh",
        vec![
            "api".into(),
            "graphql".into(),
            "-F".into(),
            "owner={owner}".into(),
            "-F".into(),
            "repo={repo}".into(),
            "-F".into(),
            format!("pr={pr}"),
            "-f".into(),
            format!("query={query}"),
        ],
        workspace,
        "effective classic policy",
    )?;
    let classic: Value =
        serde_json::from_str(&response.stdout).map_err(|_| unreadable("classic policy JSON"))?;
    if classic.get("errors").is_some() {
        return Err(unreadable("classic policy errors"));
    }
    let policy_pr = &classic["data"]["repository"]["pullRequest"];
    same(&policy_pr["headRefOid"], json!(head), "policy head")?;
    same(&policy_pr["baseRefName"], json!(base), "policy base")?;
    if !policy_pr["baseRef"].is_object()
        || policy_pr["baseRef"].get("branchProtectionRule").is_none()
    {
        return Err(unreadable("classic branch rule"));
    }
    match policy_pr.get("reviewDecision") {
        Some(Value::Null) => {}
        Some(Value::String(decision)) if decision == "APPROVED" => {}
        _ => return Err(unreadable("required or unreadable review decision")),
    }
    let mut required = Vec::new();
    let rule = &policy_pr["baseRef"]["branchProtectionRule"];
    if !rule.is_null() {
        for check in rule["requiredStatusChecks"]
            .as_array()
            .ok_or_else(|| unreadable("classic required contexts"))?
        {
            let app = check
                .get("app")
                .ok_or_else(|| unreadable("classic integration identity"))?;
            required.push(RequiredGate {
                context: text(check, "context")?.into(),
                app_id: if app.is_null() {
                    None
                } else {
                    Some(number(app, "databaseId")?)
                },
            });
        }
    }
    let rules = api(
        deadline,
        workspace,
        &format!(
            "repos/{{owner}}/{{repo}}/rules/branches/{}?per_page=100",
            encoded(base)
        ),
    )?;
    let rules_list = rules
        .as_array()
        .filter(|rules| rules.len() < 100)
        .ok_or_else(|| unreadable("truncated effective rules"))?;
    for rule in rules_list {
        let kind = text(rule, "type")?;
        if !matches!(
            kind,
            "required_status_checks"
                | "non_fast_forward"
                | "deletion"
                | "required_linear_history"
                | "pull_request"
        ) {
            return Err(unreadable(
                "an additional administrative rule may block completion",
            ));
        }
        if kind == "required_status_checks" {
            number(rule, "ruleset_id")?;
            text(rule, "ruleset_source")?;
            text(rule, "ruleset_source_type")?;
            for check in rule["parameters"]["required_status_checks"]
                .as_array()
                .ok_or_else(|| unreadable("effective required checks"))?
            {
                let integration = check
                    .get("integration_id")
                    .ok_or_else(|| unreadable("required integration"))?;
                let app_id = if integration.is_null() || integration.as_i64() == Some(-1) {
                    None
                } else {
                    Some(
                        integration
                            .as_u64()
                            .filter(|n| *n > 0)
                            .ok_or_else(|| unreadable("required integration"))?,
                    )
                };
                let gate = RequiredGate {
                    context: text(check, "context")?.into(),
                    app_id,
                };
                if !required.contains(&gate) {
                    required.push(gate);
                }
            }
        }
    }
    if required.is_empty() {
        return Err(unreadable("no effective protected context"));
    }
    let checks = api(
        deadline,
        workspace,
        &format!("repos/{{owner}}/{{repo}}/commits/{head}/check-runs?per_page=100&filter=latest"),
    )?;
    for check in complete_list(&checks, "check_runs")? {
        same(&check["head_sha"], json!(head), "check head")?;
        number(check, "id")?;
        text(check, "name")?;
        number(&check["app"], "id")?;
    }
    let runs = api(
        deadline,
        workspace,
        &format!("repos/{{owner}}/{{repo}}/actions/runs?head_sha={head}&per_page=100"),
    )?;
    complete_list(&runs, "workflow_runs")?;
    Ok(Snapshot {
        repository,
        repository_id,
        pr: pull,
        required,
        policy: json!({"classic":classic,"effective_rules":rules}),
        checks,
        runs,
    })
}

fn checked_run(
    deadline: Deadline,
    workspace: &Path,
    snapshot: &Snapshot,
    run: &Value,
) -> Result<Value, OrbitError> {
    // Snapshot has already pinned these fields to the requested completion.
    let head = text(&snapshot.pr["head"], "sha")?;
    let branch = text(&snapshot.pr["head"], "ref")?;
    let base = text(&snapshot.pr["base"], "ref")?;
    let run_id = number(run, "id")?;
    let current = api(
        deadline,
        workspace,
        &format!("repos/{{owner}}/{{repo}}/actions/runs/{run_id}"),
    )?;
    same(
        &current["run_attempt"],
        run["run_attempt"].clone(),
        "latest attempt",
    )?;
    same(
        &current["workflow_id"],
        run["workflow_id"].clone(),
        "workflow",
    )?;
    same(&current["id"], json!(run_id), "run")?;
    same(
        &current["repository"]["id"],
        json!(snapshot.repository_id),
        "run repository",
    )?;
    same(
        &current["head_repository"]["id"],
        json!(snapshot.repository_id),
        "run head repository",
    )?;
    same(&current["head_sha"], json!(head), "run head")?;
    same(&current["head_branch"], json!(branch), "run branch")?;
    same(&current["event"], json!("pull_request"), "workflow event")?;
    let pulls = current["pull_requests"]
        .as_array()
        .filter(|pulls| pulls.len() == 1)
        .ok_or_else(|| unreadable("run PR linkage"))?;
    same(&pulls[0]["number"], snapshot.pr["number"].clone(), "run PR")?;
    same(&pulls[0]["head"]["sha"], json!(head), "run PR head")?;
    same(&pulls[0]["base"]["ref"], json!(base), "run PR base")?;
    same(
        &pulls[0]["base"]["repo"]["id"],
        json!(snapshot.repository_id),
        "run PR repository",
    )?;
    number(&current, "run_attempt")?;
    number(&current, "check_suite_id")?;
    number(&current["actor"], "id")?;
    same(&current["actor"], run["actor"].clone(), "original actor")?;
    Ok(current)
}
fn workflow(
    deadline: Deadline,
    workspace: &Path,
    run: &Value,
) -> Result<(String, String, String), OrbitError> {
    let id = number(run, "workflow_id")?;
    let workflow = api(
        deadline,
        workspace,
        &format!("repos/{{owner}}/{{repo}}/actions/workflows/{id}"),
    )?;
    same(&workflow["id"], json!(id), "workflow identity")?;
    let path = text(&workflow, "path")?.to_string();
    let parts = path.split('/').collect::<Vec<_>>();
    if parts.len() != 3
        || parts[0] != ".github"
        || parts[1] != "workflows"
        || parts[2].is_empty()
        || parts[2].contains("..")
        || !(path.ends_with(".yml") || path.ends_with(".yaml"))
    {
        return Err(unreadable("workflow path"));
    }
    same(&run["path"], json!(path), "run workflow source")?;
    let head = text(run, "head_sha")?;
    let contents = api(
        deadline,
        workspace,
        &format!("repos/{{owner}}/{{repo}}/contents/{path}?ref={head}"),
    )?;
    same(&contents["type"], json!("file"), "workflow file")?;
    same(&contents["path"], json!(path), "workflow file path")?;
    let blob = text(&contents, "sha")?.to_string();
    // pull_request executes the merge ref. Requiring the event base and PR
    // head to carry the identical workflow blob proves the merge cannot
    // substitute a different DAG. Changed workflow definitions hold.
    let source_base = text(&run["pull_requests"][0]["base"], "sha")?;
    if !super::input::valid_expected_remote_sha(Some(source_base)) {
        return Err(unreadable("workflow event base commit"));
    }
    let base_contents = api(
        deadline,
        workspace,
        &format!("repos/{{owner}}/{{repo}}/contents/{path}?ref={source_base}"),
    )?;
    same(
        &base_contents["sha"],
        json!(blob),
        "workflow event-base source",
    )?;
    same(
        &base_contents["type"],
        json!("file"),
        "workflow event-base file",
    )?;
    same(
        &base_contents["path"],
        json!(path),
        "workflow event-base path",
    )?;
    let revision = format!("{head}:{path}");
    let local_blob = deadline.execute(
        "git",
        vec!["rev-parse".into(), revision.clone()],
        workspace,
        "workflow blob",
    )?;
    if local_blob.stdout.trim() != blob {
        return Err(unreadable("source blob differs from the run head"));
    }
    let source = deadline
        .execute(
            "git",
            vec!["show".into(), revision],
            workspace,
            "workflow source",
        )?
        .stdout;
    if source.len() > 64 * 1024 || contents["size"].as_u64() != Some(source.len() as u64) {
        return Err(unreadable("truncated workflow source"));
    }
    Ok((path, blob, source))
}
fn gate_passed(gate: &RequiredGate, checks: &[Value]) -> bool {
    checks.iter().any(|check| {
        check["name"].as_str() == Some(&gate.context)
            && check["status"] == "completed"
            && check["conclusion"] == "success"
            && gate
                .app_id
                .is_none_or(|app| check["app"]["id"].as_u64() == Some(app))
    })
}

/// Diagnose only an absent protected context with a static failed dependency.
pub(super) fn evidence(input: &Value) -> Result<Value, OrbitError> {
    let workspace = Path::new(required_string(input, "workspace_path")?);
    let pr = required_string(input, "pr")?;
    let head = required_string(input, "head_sha")?;
    let branch = required_string(input, "head_branch")?;
    let base = required_string(input, "base_branch")?;
    let deadline = Deadline::from_input(input)?;
    let snapshot = snapshot(deadline, workspace, pr, head, branch, base)?;
    let checks = complete_list(&snapshot.checks, "check_runs")?;
    let missing = snapshot
        .required
        .iter()
        .filter(|gate| {
            !checks.iter().any(|check| {
                check["name"].as_str() == Some(&gate.context)
                    && gate
                        .app_id
                        .is_none_or(|app| check["app"]["id"].as_u64() == Some(app))
            })
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(json!({"evidence":null}));
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
        if run["status"] != "completed" || run["conclusion"] != "failure" {
            return Err(unreadable("running, superseded or non-failed attempt"));
        }
        let run_id = number(&run, "id")?;
        let attempt = number(&run, "run_attempt")?;
        let jobs = api(
            deadline,
            workspace,
            &format!(
                "repos/{{owner}}/{{repo}}/actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100"
            ),
        )?;
        let job_list = complete_list(&jobs, "jobs")?;
        let mut job_ids = std::collections::BTreeSet::new();
        for job in job_list {
            if !job_ids.insert(number(job, "id")?) {
                return Err(unreadable("duplicate provider job identity"));
            }
            same(&job["run_id"], json!(run_id), "job run")?;
            same(&job["head_sha"], json!(head), "job head")?;
            same(&job["run_attempt"], json!(attempt), "job attempt")?;
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
            let Ok(dependency) = dependency(&source, &gate.context, text(failed, "name")?) else {
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
            super::workflow_read::api_step(
                &dependency.workflow,
                &dependency.prerequisite,
                text(failed_step, "name")?,
            )?;
            let suite = api(
                deadline,
                workspace,
                &format!(
                    "repos/{{owner}}/{{repo}}/check-suites/{}",
                    number(&run, "check_suite_id")?
                ),
            )?;
            same(&suite["id"], run["check_suite_id"].clone(), "suite")?;
            same(&suite["head_sha"], json!(head), "suite head")?;
            same(
                &suite["repository"]["id"],
                json!(snapshot.repository_id),
                "suite repository",
            )?;
            same(
                &suite["app"]["slug"],
                json!("github-actions"),
                "workflow integration",
            )?;
            let app_id = number(&suite["app"], "id")?;
            if gate.app_id.is_some_and(|app| app != app_id) {
                return Err(unreadable("protected integration differs"));
            }
            super::workflow_dependency::certify_rerun(
                &dependency,
                job_list,
                &snapshot.required,
                app_id,
            )?;
            let job_id = number(failed, "id")?;
            let check_url = text(failed, "check_run_url")?;
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
            if log["truncated"] != false || !log["fallback_error"].is_null() {
                return Err(unreadable("truncated/unreadable prerequisite log"));
            }
            let log = text(&log, "log")?;
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
                base_sha: text(&snapshot.pr["base"], "sha")?.into(),
                required: snapshot.required.clone(),
                missing: (*gate).clone(),
                app_id,
                workflow_id: number(&run, "workflow_id")?,
                workflow_path: path.clone(),
                workflow_blob: blob.clone(),
                workflow_source: source.clone(),
                workflow_job: dependency.job,
                prerequisite: dependency.prerequisite,
                prerequisite_job_id: job_id,
                run_id,
                run_attempt: u32::try_from(attempt).map_err(|_| unreadable("attempt overflow"))?,
                event: "pull_request".into(),
                policy: snapshot.policy.clone(),
                provider: json!({"pr":snapshot.pr,"checks":snapshot.checks,"suite":suite,"workflow_runs":snapshot.runs}),
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

/// Observe the reserved attempt and all current gates before any completion.
/// A missing gate is never inferred to pass from the PR's CLEAN state.
pub(super) fn observe(input: &Value) -> Result<Value, OrbitError> {
    let workspace = Path::new(required_string(input, "workspace_path")?);
    let evidence: PrInfrastructureEvidence = serde_json::from_value(input["evidence"].clone())
        .map_err(|_| unreadable("reserved evidence"))?;
    let deadline = Deadline::from_input(input)?;
    let snapshot = snapshot(
        deadline,
        workspace,
        &evidence.pr_number,
        &evidence.head_sha,
        &evidence.head_branch,
        &evidence.base_branch,
    )?;
    if snapshot.repository != evidence.repository
        || snapshot.repository_id != evidence.repository_id
        || snapshot.required != evidence.required
        || snapshot.pr["base"]["sha"] != evidence.base_sha
    {
        return Err(unreadable(
            "policy/repository/base changed since reservation",
        ));
    }
    let runs = complete_list(&snapshot.runs, "workflow_runs")?;
    if runs.iter().any(|run| {
        run["workflow_id"].as_u64() == Some(evidence.workflow_id)
            && run["id"].as_u64().is_none_or(|id| id > evidence.run_id)
    }) {
        return Err(unreadable("reserved workflow run was superseded"));
    }
    let listed = runs
        .iter()
        .find(|run| run["id"].as_u64() == Some(evidence.run_id))
        .ok_or_else(|| unreadable("reserved run disappeared"))?;
    let run = checked_run(deadline, workspace, &snapshot, listed)?;
    if number(&run, "workflow_id")? != evidence.workflow_id
        || text(&run, "path")? != evidence.workflow_path
        || text(&run, "event")? != evidence.event
        || run["actor"] != evidence.run["actor"]
    {
        return Err(unreadable("reserved workflow/event changed"));
    }
    let attempt = number(&run, "run_attempt")?;
    if attempt < u64::from(evidence.run_attempt) || attempt > u64::from(evidence.run_attempt) + 1 {
        return Err(unreadable("superseded/ambiguous reserved attempt"));
    }
    let checks = complete_list(&snapshot.checks, "check_runs")?;
    let passed = snapshot
        .required
        .iter()
        .all(|gate| gate_passed(gate, checks))
        && checks.iter().any(|check| {
            check["name"].as_str() == Some(&evidence.missing.context)
                && check["app"]["id"].as_u64() == Some(evidence.app_id)
                && check["check_suite"]["id"] == run["check_suite_id"]
                && check["status"] == "completed"
                && check["conclusion"] == "success"
        });
    let state = text(&run, "status")?;
    if !matches!(
        state,
        "completed" | "queued" | "in_progress" | "waiting" | "requested" | "pending"
    ) {
        return Err(unreadable("unknown attempt state"));
    }
    if attempt == u64::from(evidence.run_attempt)
        && (state != "completed" || run["conclusion"] != "failure")
    {
        return Err(unreadable("original attempt changed without a rerun"));
    }
    if attempt > u64::from(evidence.run_attempt)
        && state == "completed"
        && (run["conclusion"] != "success" || !passed)
    {
        return Err(unreadable(
            "second failure or protected check still missing",
        ));
    }
    deadline.remaining()?;
    Ok(
        json!({"attempt":attempt,"status":state,"conclusion":run["conclusion"],"gates_passed": passed && attempt > u64::from(evidence.run_attempt) && state == "completed", "run":run,"checks":snapshot.checks}),
    )
}
