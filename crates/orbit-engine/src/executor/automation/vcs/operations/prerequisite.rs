//! GitHub owns effective policy, workflow identities and source dependency reads.
//! The engine receives verified generic evidence; no workflow is special-cased.
use super::super::completion_deadline::Deadline;
use super::{
    MAX_WORKFLOW_SOURCE_BYTES,
    ci::ci_job_log,
    input::{number, required_string, same, text},
    workflow_dependency::dependency,
};
use orbit_common::OrbitError;
use orbit_types::workflow::{PrInfrastructureEvidence, RequiredGate};
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn unreadable(reason: &str) -> OrbitError {
    OrbitError::workflow_evidence_unavailable(reason)
}
pub(super) fn api(
    deadline: Deadline,
    workspace: &Path,
    endpoint: &str,
) -> Result<Value, OrbitError> {
    let response = deadline.execute(
        "gh",
        vec!["api".into(), endpoint.into()],
        workspace,
        &format!("workflow prerequisite read {endpoint}"),
    )?;
    serde_json::from_str(&response.stdout).map_err(|_| unreadable("invalid API JSON"))
}
pub(super) fn complete_list<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>, OrbitError> {
    let list = value[key].as_array().ok_or_else(|| unreadable(key))?;
    if value["total_count"].as_u64() != Some(list.len() as u64) || list.len() > 100 {
        return Err(unreadable(&format!("truncated {key}")));
    }
    Ok(list)
}
pub(super) fn encoded(value: &str) -> String {
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

pub(super) struct Snapshot {
    pub(super) repository: String,
    pub(super) repository_id: u64,
    pub(super) pr: Value,
    pub(super) required: Vec<RequiredGate>,
    pub(super) policy: Value,
    pub(super) checks: Value,
    pub(super) statuses: Value,
    pub(super) runs: Value,
}
/// First wait between reads of unreadable-but-possibly-transient evidence.
const FIRST_RETRY_WAIT_MS: u64 = 5_000;

/// Poll interval a snapshot's retry wait grows to when the caller names none:
/// the completion wait's default.
const DEFAULT_POLL_INTERVAL_MS: u64 = 30_000;

/// The one classification of unreadable causes. Every cause not listed here is
/// transient: the provider may simply not have reported it yet (a head that
/// lags the pull request, a check or status not reported), so the snapshot
/// reads again with a growing wait. Each listed cause cannot change within
/// seconds, and some not at all without a new head or an operator, so the
/// snapshot returns it at once instead of spending REST quota on retries.
const NON_TRANSIENT_CAUSES: &[(&str, &str)] = &[
    (
        "PR/head selector",
        "the selector does not parse, and a read cannot repair it",
    ),
    (
        "PR number",
        "the pull request number does not parse, and a read cannot repair it",
    ),
    (
        "truncated check_runs",
        "a list over 100 entries only grows while the head stands",
    ),
    (
        "truncated statuses",
        "a list over 100 entries only grows while the head stands",
    ),
    (
        "truncated workflow_runs",
        "a list over 100 entries only grows while the head stands",
    ),
    (
        "truncated effective rules",
        "a rule list over 100 entries only grows until an operator trims it",
    ),
    (
        "classic branch rule",
        "the classic branch rule shape is one the reader does not accept",
    ),
    (
        "classic required contexts",
        "the classic required-context shape is one the reader does not accept",
    ),
    (
        "classic integration identity",
        "the classic integration identity shape is one the reader does not accept",
    ),
    (
        "effective required checks",
        "the ruleset's required-check shape is one the reader does not accept",
    ),
    (
        "required integration",
        "the ruleset's integration identity is one the reader does not accept",
    ),
    (
        "ruleset_id",
        "the ruleset shape is one the reader does not accept",
    ),
    (
        "ruleset_source",
        "the ruleset shape is one the reader does not accept",
    ),
    (
        "ruleset_source_type",
        "the ruleset shape is one the reader does not accept",
    ),
    (
        "an additional administrative rule may block completion",
        "a ruleset the reader does not accept stays until an operator changes it",
    ),
    (
        "required or unreadable review decision",
        "a review decision that is neither approved nor absent waits on a reviewer, not on a retry",
    ),
];

/// Why `error` is an unreadable cause no retry can clear, or `None` when it is
/// transient (or not an unreadable-evidence error at all).
fn non_transient_reason(error: &OrbitError) -> Option<&'static str> {
    let OrbitError::WorkflowEvidenceUnavailable(evidence) = error else {
        return None;
    };
    NON_TRANSIENT_CAUSES
        .iter()
        .find(|(cause, _)| *cause == evidence.reason)
        .map(|(_, why)| *why)
}

/// The caller's poll interval, in milliseconds: the optional
/// `poll_interval_seconds` input within the completion wait's bounds
/// (5 s to 10 min), else its 30 s default.
pub(super) fn poll_interval_ms(input: &Value) -> u64 {
    input
        .get("poll_interval_seconds")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_POLL_INTERVAL_MS, |seconds| {
            seconds.clamp(5, 600) * 1_000
        })
}

/// Read the current-gates evidence once it is readable. An unreadable cause
/// that cannot change returns at once; a transient one is read again after a
/// wait that starts at 5 s and doubles up to `poll_interval_ms`, until the
/// deadline ends the wait with the last unreadable cause.
pub(super) fn snapshot(
    deadline: Deadline,
    workspace: &Path,
    pr: &str,
    head: &str,
    branch: &str,
    base: &str,
    poll_interval_ms: u64,
) -> Result<Snapshot, OrbitError> {
    if pr.is_empty()
        || !pr.bytes().all(|byte| byte.is_ascii_digit())
        || !super::input::valid_expected_remote_sha(Some(head))
    {
        return Err(unreadable("PR/head selector"));
    }
    let mut previous_unreadable = None;
    let mut throttle_waits = 0;
    let wait_cap = poll_interval_ms.max(1);
    let mut next_wait = FIRST_RETRY_WAIT_MS.min(wait_cap);
    loop {
        let error = match snapshot_once(deadline, workspace, pr, head, branch, base) {
            Ok(snapshot) => return Ok(snapshot),
            Err(error) => error,
        };
        // [ORB-25864] A rate-limited read waits for the provider's reset
        // within the deadline and reads the whole snapshot again.
        if super::read_reset::wait_for_reset(deadline, workspace, &error, &mut throttle_waits) {
            continue;
        }
        // An in-flight retry can itself run out of time. Keep the last
        // unavailable snapshot's cause when that happens as well.
        if deadline.remaining().is_err() {
            return Err(previous_unreadable.unwrap_or(error));
        }
        // Only this adapter's unavailable-evidence errors are retried here.
        // Transport/authentication failures retain their provider class and
        // existing recovery policy. Never act on a partial snapshot.
        if !matches!(&error, OrbitError::WorkflowEvidenceUnavailable(_)) {
            return Err(error);
        }
        // [ORB-25870] A cause a read cannot clear is not worth another read.
        if let Some(why) = non_transient_reason(&error) {
            tracing::debug!(
                target: "orbit_engine::executor::automation::vcs::operations",
                reason = why,
                "workflow prerequisite evidence is unreadable and will not change on a retry"
            );
            return Err(error);
        }
        // Unreadable evidence may come from a stale repository identity.
        super::read_cache::forget_repository_identity(workspace);
        // The wait is bounded by the deadline itself, not by one read's cap.
        let Ok(remaining) = deadline.remaining_uncapped() else {
            return Err(error);
        };
        super::super::remote_probe::fixed_wait(std::time::Duration::from_millis(
            remaining.min(next_wait),
        ));
        next_wait = next_wait.saturating_mul(2).min(wait_cap);
        // Keep the original unreadable-evidence diagnostic at exhaustion,
        // rather than replacing it with a generic deadline error.
        if deadline.remaining().is_err() {
            return Err(error);
        }
        previous_unreadable = Some(error);
    }
}

pub(super) fn snapshot_once(
    deadline: Deadline,
    workspace: &Path,
    pr: &str,
    head: &str,
    branch: &str,
    base: &str,
) -> Result<Snapshot, OrbitError> {
    let repo = super::read_cache::repository_identity(deadline, workspace)?;
    let repository = text(&repo, "full_name", unreadable)?.to_string();
    let repository_id = number(&repo, "id", unreadable)?;
    let pull = api(
        deadline,
        workspace,
        &format!("repos/{{owner}}/{{repo}}/pulls/{pr}"),
    )?;
    same(
        &pull["number"],
        &json!(pr.parse::<u64>().map_err(|_| unreadable("PR number"))?),
        "PR",
        unreadable,
    )?;
    same(&pull["state"], &json!("open"), "PR state", unreadable)?;
    same(&pull["merged"], &json!(false), "merged PR", unreadable)?;
    for side in ["head", "base"] {
        same(
            &pull[side]["repo"]["id"],
            &json!(repository_id),
            "repository",
            unreadable,
        )?;
        same(
            &pull[side]["repo"]["full_name"],
            &json!(repository),
            "repository name",
            unreadable,
        )?;
    }
    same(&pull["head"]["sha"], &json!(head), "head", unreadable)?;
    same(&pull["head"]["ref"], &json!(branch), "branch", unreadable)?;
    same(&pull["base"]["ref"], &json!(base), "base", unreadable)?;
    // GraphQL supplies the effective classic rule for this exact base. The
    // active branch-rules endpoint adds organization/repository rulesets.
    let query = "query($pr:Int!,$owner:String!,$repo:String!){repository(owner:$owner,name:$repo){pullRequest(number:$pr){headRefOid baseRefName reviewDecision baseRef{branchProtectionRule{id requiredStatusChecks{context app{databaseId slug}}}}}}}";
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
    same(
        &policy_pr["headRefOid"],
        &json!(head),
        "policy head",
        unreadable,
    )?;
    same(
        &policy_pr["baseRefName"],
        &json!(base),
        "policy base",
        unreadable,
    )?;
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
                context: text(check, "context", unreadable)?.into(),
                app_id: if app.is_null() {
                    None
                } else {
                    Some(number(app, "databaseId", unreadable)?)
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
        let kind = text(rule, "type", unreadable)?;
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
            number(rule, "ruleset_id", unreadable)?;
            text(rule, "ruleset_source", unreadable)?;
            text(rule, "ruleset_source_type", unreadable)?;
            for check in rule["parameters"]["required_status_checks"]
                .as_array()
                .ok_or_else(|| unreadable("effective required checks"))?
            {
                let app_id = match check.get("integration_id") {
                    None => None,
                    Some(integration)
                        if integration.is_null() || integration.as_i64() == Some(-1) =>
                    {
                        None
                    }
                    Some(integration) => Some(
                        integration
                            .as_u64()
                            .filter(|n| *n > 0)
                            .ok_or_else(|| unreadable("required integration"))?,
                    ),
                };
                let gate = RequiredGate {
                    context: text(check, "context", unreadable)?.into(),
                    app_id,
                };
                if !required.contains(&gate) {
                    required.push(gate);
                }
            }
        }
    }
    // A successfully read empty policy is valid for current-gate observation.
    // Recovery callers must still prove an actual missing workflow context.
    let checks = api(
        deadline,
        workspace,
        &format!("repos/{{owner}}/{{repo}}/commits/{head}/check-runs?per_page=100&filter=latest"),
    )?;
    for check in complete_list(&checks, "check_runs")? {
        same(&check["head_sha"], &json!(head), "check head", unreadable)?;
        number(check, "id", unreadable)?;
        text(check, "name", unreadable)?;
        number(&check["app"], "id", unreadable)?;
    }
    // Combined status pins the latest legacy contexts to this repository and
    // commit. A context has no app identity and cannot satisfy an app-bound gate.
    let statuses = api(
        deadline,
        workspace,
        &format!("repos/{{owner}}/{{repo}}/commits/{head}/status?per_page=100"),
    )?;
    same(&statuses["sha"], &json!(head), "status head", unreadable)?;
    same(
        &statuses["repository"]["id"],
        &json!(repository_id),
        "status repository",
        unreadable,
    )?;
    same(
        &statuses["repository"]["full_name"],
        &json!(repository),
        "status repository name",
        unreadable,
    )?;
    for status in complete_list(&statuses, "statuses")? {
        number(status, "id", unreadable)?;
        text(status, "context", unreadable)?;
        text(status, "state", unreadable)?;
        same(
            &status["url"],
            &json!(format!(
                "{}/statuses/{head}",
                text(&repo, "url", unreadable)?
            )),
            "status context repository/head",
            unreadable,
        )?;
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
        statuses,
        runs,
    })
}

pub(super) fn checked_run(
    deadline: Deadline,
    workspace: &Path,
    snapshot: &Snapshot,
    run: &Value,
) -> Result<Value, OrbitError> {
    // Snapshot has already pinned these fields to the requested completion.
    let head = text(&snapshot.pr["head"], "sha", unreadable)?;
    let branch = text(&snapshot.pr["head"], "ref", unreadable)?;
    let base = text(&snapshot.pr["base"], "ref", unreadable)?;
    let run_id = number(run, "id", unreadable)?;
    let current = api(
        deadline,
        workspace,
        &format!("repos/{{owner}}/{{repo}}/actions/runs/{run_id}"),
    )?;
    same(
        &current["run_attempt"],
        &run["run_attempt"],
        "latest attempt",
        unreadable,
    )?;
    same(
        &current["workflow_id"],
        &run["workflow_id"],
        "workflow",
        unreadable,
    )?;
    same(&current["id"], &json!(run_id), "run", unreadable)?;
    same(
        &current["repository"]["id"],
        &json!(snapshot.repository_id),
        "run repository",
        unreadable,
    )?;
    same(
        &current["head_repository"]["id"],
        &json!(snapshot.repository_id),
        "run head repository",
        unreadable,
    )?;
    same(&current["head_sha"], &json!(head), "run head", unreadable)?;
    same(
        &current["head_branch"],
        &json!(branch),
        "run branch",
        unreadable,
    )?;
    same(
        &current["event"],
        &json!("pull_request"),
        "workflow event",
        unreadable,
    )?;
    let pulls = current["pull_requests"]
        .as_array()
        .filter(|pulls| pulls.len() == 1)
        .ok_or_else(|| unreadable("run PR linkage"))?;
    same(
        &pulls[0]["number"],
        &snapshot.pr["number"],
        "run PR",
        unreadable,
    )?;
    same(
        &pulls[0]["head"]["sha"],
        &json!(head),
        "run PR head",
        unreadable,
    )?;
    same(
        &pulls[0]["base"]["ref"],
        &json!(base),
        "run PR base",
        unreadable,
    )?;
    same(
        &pulls[0]["base"]["repo"]["id"],
        &json!(snapshot.repository_id),
        "run PR repository",
        unreadable,
    )?;
    number(&current, "run_attempt", unreadable)?;
    number(&current, "check_suite_id", unreadable)?;
    number(&current["actor"], "id", unreadable)?;
    same(
        &current["actor"],
        &run["actor"],
        "original actor",
        unreadable,
    )?;
    Ok(current)
}
pub(super) fn workflow(
    deadline: Deadline,
    workspace: &Path,
    run: &Value,
) -> Result<(String, String, String), OrbitError> {
    let id = number(run, "workflow_id", unreadable)?;
    let workflow = api(
        deadline,
        workspace,
        &format!("repos/{{owner}}/{{repo}}/actions/workflows/{id}"),
    )?;
    same(&workflow["id"], &json!(id), "workflow identity", unreadable)?;
    let path = text(&workflow, "path", unreadable)?.to_string();
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
    same(
        &run["path"],
        &json!(path),
        "run workflow source",
        unreadable,
    )?;
    let head = text(run, "head_sha", unreadable)?;
    let contents = super::read_cache::contents_at_commit(deadline, workspace, &path, head)?;
    same(
        &contents["type"],
        &json!("file"),
        "workflow file",
        unreadable,
    )?;
    same(
        &contents["path"],
        &json!(path),
        "workflow file path",
        unreadable,
    )?;
    let blob = text(&contents, "sha", unreadable)?.to_string();
    // pull_request executes the merge ref. Requiring the event base and PR
    // head to carry the identical workflow blob proves the merge cannot
    // substitute a different DAG. Changed workflow definitions hold.
    let source_base = text(&run["pull_requests"][0]["base"], "sha", unreadable)?;
    if !super::input::valid_expected_remote_sha(Some(source_base)) {
        return Err(unreadable("workflow event base commit"));
    }
    let base_contents =
        super::read_cache::contents_at_commit(deadline, workspace, &path, source_base)?;
    same(
        &base_contents["sha"],
        &json!(blob),
        "workflow event-base source",
        unreadable,
    )?;
    same(
        &base_contents["type"],
        &json!("file"),
        "workflow event-base file",
        unreadable,
    )?;
    same(
        &base_contents["path"],
        &json!(path),
        "workflow event-base path",
        unreadable,
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
    if source.len() > MAX_WORKFLOW_SOURCE_BYTES
        || contents["size"].as_u64() != Some(source.len() as u64)
    {
        return Err(unreadable("truncated workflow source"));
    }
    Ok((path, blob, source))
}
/// Presence, including unsuccessful evidence, prevents absent-workflow diagnosis.
pub(super) fn gate_present(gate: &RequiredGate, checks: &[Value], statuses: &[Value]) -> bool {
    checks.iter().any(|check| {
        check["name"].as_str() == Some(&gate.context)
            && gate
                .app_id
                .is_none_or(|app| check["app"]["id"].as_u64() == Some(app))
    }) || (gate.app_id.is_none()
        && statuses
            .iter()
            .any(|status| status["context"] == gate.context))
}

pub(super) fn gate_passed(gate: &RequiredGate, checks: &[Value], statuses: &[Value]) -> bool {
    // When a context has both representations, neither may conceal a failure
    // or pending result in the other. Only explicit success pays an owed gate.
    gate_present(gate, checks, statuses)
        && checks
            .iter()
            .filter(|check| {
                check["name"].as_str() == Some(&gate.context)
                    && gate
                        .app_id
                        .is_none_or(|app| check["app"]["id"].as_u64() == Some(app))
            })
            .all(|check| check["status"] == "completed" && check["conclusion"] == "success")
        && (gate.app_id.is_some()
            || statuses
                .iter()
                .filter(|status| status["context"] == gate.context)
                .all(|status| status["state"] == "success"))
}

/// Diagnose only an absent protected context with a static failed dependency.
/// A rate-limited read anywhere in the pass, after the snapshot included,
/// waits for the provider's reset within the deadline and reads the evidence
/// again; nothing here mutates.
pub(super) fn evidence(input: &Value) -> Result<Value, OrbitError> {
    let workspace = Path::new(required_string(input, "workspace_path")?);
    let deadline = Deadline::from_input(input)?;
    super::read_reset::read_through_throttles(deadline, workspace, || evidence_once(input))
}

fn evidence_once(input: &Value) -> Result<Value, OrbitError> {
    let workspace = Path::new(required_string(input, "workspace_path")?);
    let pr = required_string(input, "pr")?;
    let head = required_string(input, "head_sha")?;
    let branch = required_string(input, "head_branch")?;
    let base = required_string(input, "base_branch")?;
    let deadline = Deadline::from_input(input)?;
    let snapshot = snapshot(
        deadline,
        workspace,
        pr,
        head,
        branch,
        base,
        poll_interval_ms(input),
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
        let failures =
            super::failed_workflow::failed_workflow_evidence(deadline, workspace, &snapshot)?;
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
            let failures =
                super::failed_workflow::failed_workflow_evidence(deadline, workspace, &snapshot)?;
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
            super::workflow_read::api_step(
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
            super::workflow_dependency::certify_rerun(
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
                return Err(unreadable("truncated/unreadable prerequisite log"));
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

/// Observe the reserved attempt and all current gates before any completion.
/// A missing gate is never inferred to pass from the PR's CLEAN state.
/// Observation only reads: a rate-limited read waits for the reset within the
/// deadline and observes again, and never resends the reserved mutation.
pub(super) fn observe(input: &Value) -> Result<Value, OrbitError> {
    let workspace = Path::new(required_string(input, "workspace_path")?);
    let deadline = Deadline::from_input(input)?;
    super::read_reset::read_through_throttles(deadline, workspace, || observe_once(input))
}

fn observe_once(input: &Value) -> Result<Value, OrbitError> {
    let workspace = Path::new(required_string(input, "workspace_path")?);
    let evidence: PrInfrastructureEvidence = serde_json::from_value(input["evidence"].clone())
        .map_err(|_| unreadable("reserved evidence"))?;
    let deadline = Deadline::from_input(input)?;
    if let Some(failure) = evidence.failed_workflow_run() {
        if failure.candidate_owned {
            return Err(unreadable(
                "candidate-owned workflow failure cannot authorize infrastructure recovery",
            ));
        }
        return super::failed_workflow::observe_failed_workflow(
            deadline,
            workspace,
            &evidence,
            poll_interval_ms(input),
        );
    }
    if evidence.required.is_empty() {
        return Err(unreadable("reserved protected contexts"));
    }
    let snapshot = snapshot(
        deadline,
        workspace,
        &evidence.pr_number,
        &evidence.head_sha,
        &evidence.head_branch,
        &evidence.base_branch,
        poll_interval_ms(input),
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
    if number(&run, "workflow_id", unreadable)? != evidence.workflow_id
        || text(&run, "path", unreadable)? != evidence.workflow_path
        || text(&run, "event", unreadable)? != evidence.event
        || run["actor"] != evidence.run["actor"]
    {
        return Err(unreadable("reserved workflow/event changed"));
    }
    let attempt = number(&run, "run_attempt", unreadable)?;
    if attempt < u64::from(evidence.run_attempt) || attempt > u64::from(evidence.run_attempt) + 1 {
        return Err(unreadable("superseded/ambiguous reserved attempt"));
    }
    let checks = complete_list(&snapshot.checks, "check_runs")?;
    let statuses = complete_list(&snapshot.statuses, "statuses")?;
    let passed = snapshot
        .required
        .iter()
        .all(|gate| gate_passed(gate, checks, statuses))
        && checks.iter().any(|check| {
            check["name"].as_str() == Some(&evidence.missing.context)
                && check["app"]["id"].as_u64() == Some(evidence.app_id)
                && check["check_suite"]["id"] == run["check_suite_id"]
                && check["status"] == "completed"
                && check["conclusion"] == "success"
        });
    let state = text(&run, "status", unreadable)?;
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
        json!({"attempt":attempt,"status":state,"conclusion":run["conclusion"],"gates_passed": passed && attempt > u64::from(evidence.run_attempt) && state == "completed", "run":run,"checks":snapshot.checks,"statuses":snapshot.statuses}),
    )
}
