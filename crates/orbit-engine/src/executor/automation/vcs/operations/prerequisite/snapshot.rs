use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::RequiredGate;
use serde_json::{Value, json};

use super::super::super::completion_deadline::Deadline;
use super::super::input::{number, same, text};
use super::support::{api, complete_list, encoded, unreadable};

pub(in super::super) struct Snapshot {
    pub(in super::super) repository: String,
    pub(in super::super) repository_id: u64,
    pub(in super::super) pr: Value,
    pub(in super::super) required: Vec<RequiredGate>,
    pub(in super::super) policy: Value,
    pub(in super::super) checks: Value,
    pub(in super::super) statuses: Value,
    pub(in super::super) runs: Value,
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
pub(in super::super) fn poll_interval_ms(input: &Value) -> u64 {
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
#[allow(clippy::too_many_arguments)]
pub(in super::super) fn snapshot(
    deadline: Deadline,
    workspace: &Path,
    pr: &str,
    head: &str,
    branch: &str,
    base: &str,
    poll_interval_ms: u64,
    pass: &super::super::read_reset::SnapshotWaits,
) -> Result<Snapshot, OrbitError> {
    if pr.is_empty()
        || !pr.bytes().all(|byte| byte.is_ascii_digit())
        || !super::super::input::valid_expected_remote_sha(Some(head))
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
        if super::super::read_reset::wait_for_reset(
            deadline,
            workspace,
            &error,
            &mut throttle_waits,
        ) {
            continue;
        }
        // The snapshot's waits are spent: the enclosing pass must not read
        // the snapshot again over the same throttle.
        if super::super::read_reset::is_rate_limited(&error) {
            pass.mark_spent();
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
        super::super::read_cache::forget_repository_identity(workspace);
        // The wait is bounded by the deadline itself, not by one read's cap.
        let Ok(remaining) = deadline.remaining_uncapped() else {
            return Err(error);
        };
        super::super::super::remote_probe::fixed_wait(std::time::Duration::from_millis(
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

pub(in super::super) fn snapshot_once(
    deadline: Deadline,
    workspace: &Path,
    pr: &str,
    head: &str,
    branch: &str,
    base: &str,
) -> Result<Snapshot, OrbitError> {
    let repo = super::super::read_cache::repository_identity(deadline, workspace)?;
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

pub(in super::super) fn checked_run(
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
