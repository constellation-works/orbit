//! Turn a host-collected repository security snapshot into ordinary backlog tasks.

mod duplicates;

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::duplicate_tasks::{
    DuplicateTaskLookup, DuplicateTaskMatch, find_covering_task,
};
use crate::application::task::TaskAddParams;

use self::duplicates::{
    code_duplicate_candidate, dependabot_duplicate_candidate, duplicate_lookup_error,
    secret_duplicate_candidate,
};

const SUPPORTED_SCHEMA_VERSION: u64 = 2;
const DEPENDABOT_TAG: &str = "dependabot-sweep";
const DEPENDABOT_KEY_PREFIX: &str = "dependabot:";
const DEPENDABOT_TITLE_PREFIX: &str = "[dependabot-sweep] ";
const CODE_TAG: &str = "code-scanning-sweep";
const CODE_KEY_PREFIX: &str = "code-scanning:";
const CODE_TITLE_PREFIX: &str = "[code-scanning-sweep] ";
const SECRET_TAG: &str = "secret-scanning-sweep";
const SECRET_KEY_PREFIX: &str = "secret-scanning:";
const SECRET_TITLE_PREFIX: &str = "[secret-scanning-sweep] ";
const SYSTEM_CREW: &str = "system";
const DEFAULT_MAX_TASKS: u64 = 10;
const MAX_TASKS: u64 = 50;
const KEY_LEN: usize = 16;

struct PendingTask {
    params: TaskAddParams,
    result: Value,
}

pub(crate) fn file_dependabot_alert_tasks(
    runtime: &OrbitRuntime,
    input: &Value,
) -> Result<Value, OrbitError> {
    file_dependabot_alert_tasks_with_lookup(runtime, input, runtime)
}

pub(in crate::adapter::engine_host::v2_host) fn file_dependabot_alert_tasks_with_lookup<L>(
    runtime: &OrbitRuntime,
    input: &Value,
    lookup: &L,
) -> Result<Value, OrbitError>
where
    L: DuplicateTaskLookup + ?Sized,
{
    let snapshot = input.get("dependabot_snapshot").ok_or_else(|| {
        OrbitError::InvalidInput(
            "file_dependabot_alert_tasks requires the `dependabot_snapshot` produced by collect_dependabot_alerts"
                .to_string(),
        )
    })?;
    if !snapshot.is_object() {
        return Err(OrbitError::InvalidInput(
            "dependabot_snapshot must be an object".to_string(),
        ));
    }
    let version = snapshot
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if version > SUPPORTED_SCHEMA_VERSION {
        return Err(OrbitError::InvalidInput(format!(
            "security-alert snapshot schema version {version} is newer than supported version {SUPPORTED_SCHEMA_VERSION}"
        )));
    }

    let floor_name = input
        .get("min_severity")
        .and_then(Value::as_str)
        .unwrap_or("high")
        .trim()
        .to_ascii_lowercase();
    let floor = severity_rank(&floor_name).ok_or_else(|| {
        OrbitError::InvalidInput(
            "input.min_severity must be one of low, moderate, high, or critical".to_string(),
        )
    })?;
    let max_tasks = bounded_u64(input, "max_tasks", DEFAULT_MAX_TASKS, MAX_TASKS)? as usize;
    let skip_pr = input
        .get("skip_when_dependabot_pr_open")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let system_crew = runtime
        .validate_crew_name(Some(SYSTEM_CREW))
        .is_ok()
        .then(|| SYSTEM_CREW.to_string());

    let mut pending_tasks = Vec::new();
    let mut skipped_existing = Vec::new();
    let mut skipped_dependabot_pr = Vec::new();
    let mut skipped_over_cap = Vec::new();
    let mut excluded_below_min_severity = Vec::new();
    let mut candidate_count = 0usize;

    if snapshot.get("collected").and_then(Value::as_bool) == Some(true) {
        let alerts = snapshot
            .get("open_alerts")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut grouped: BTreeMap<(String, String, String), Vec<Value>> = BTreeMap::new();
        for alert in alerts {
            let severity = field(alert, "severity").to_ascii_lowercase();
            let rank = severity_rank(&severity).unwrap_or(0);
            if rank < floor {
                excluded_below_min_severity.push(json!({
                    "family": "dependabot",
                    "number": alert.get("number"),
                    "ecosystem": field(alert, "ecosystem"),
                    "package": field(alert, "package"),
                    "manifest_path": field(alert, "manifest_path"),
                    "severity": severity,
                }));
                continue;
            }
            let key = (
                field(alert, "ecosystem"),
                field(alert, "package"),
                field(alert, "manifest_path"),
            );
            if key.0.is_empty() || key.1.is_empty() || key.2.is_empty() {
                continue;
            }
            grouped.entry(key).or_default().push(alert.clone());
        }

        let pull_requests = snapshot
            .get("open_dependabot_pull_requests")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        candidate_count += grouped.len();
        for ((ecosystem, package, manifest_path), mut cluster_alerts) in grouped {
            cluster_alerts
                .sort_by_key(|alert| alert.get("number").and_then(Value::as_u64).unwrap_or(0));
            let key = digest(&[&ecosystem, &package, &manifest_path]);
            if let Some(DuplicateTaskMatch {
                task_id,
                match_kind,
                evidence,
            }) = find_covering_task(
                lookup,
                &dependabot_duplicate_candidate(&key, &package, &manifest_path),
            )
            .map_err(|error| duplicate_lookup_error("dependabot", &key, &error))?
            {
                skipped_existing.push(json!({
                    "family": "dependabot", "key": key, "task_id": task_id,
                    "ecosystem": ecosystem, "package": package, "manifest_path": manifest_path,
                    "match_kind": match_kind, "match_evidence": evidence,
                }));
                continue;
            }
            if skip_pr
                && let Some(pull_request) = pull_requests.iter().find(|pull_request| {
                    pull_request_bumps_package(pull_request, &ecosystem, &package)
                })
            {
                skipped_dependabot_pr.push(json!({
                    "family": "dependabot", "key": key, "ecosystem": ecosystem,
                    "package": package, "manifest_path": manifest_path,
                    "pull_request": pull_request,
                }));
                continue;
            }
            if pending_tasks.len() >= max_tasks {
                skipped_over_cap.push(json!({
                    "family": "dependabot", "key": key, "ecosystem": ecosystem,
                    "package": package, "manifest_path": manifest_path,
                }));
                continue;
            }

            let highest = cluster_alerts
                .iter()
                .filter_map(|alert| severity_rank(&field(alert, "severity").to_ascii_lowercase()))
                .max()
                .unwrap_or(floor);
            let params = TaskAddParams {
                title: dependabot_task_title(&package, &manifest_path),
                description: dependabot_task_description(
                    snapshot,
                    &ecosystem,
                    &package,
                    &manifest_path,
                    &cluster_alerts,
                ),
                acceptance_criteria: vec![
                    format!("Bump `{package}` to a non-vulnerable version that resolves every alert listed in the task description."),
                    format!("`{manifest_path}` and the repository lockfile agree on the resolved `{package}` version."),
                    "The repository's documented pre-handoff checks pass without weakening security checks or suppressing the advisories.".to_string(),
                ],
                tags: vec![
                    DEPENDABOT_TAG.to_string(),
                    format!("{DEPENDABOT_KEY_PREFIX}{key}"),
                    "security".to_string(),
                ],
                required_tools: Vec::new(),
                crew: system_crew.clone(),
                priority: priority_for_rank(highest),
                complexity: TaskComplexity::Unassessed,
                task_type: Some(TaskType::Bug),
                status: Some(TaskStatus::Backlog),
                system_created: true,
                ..TaskAddParams::default()
            };
            pending_tasks.push(PendingTask {
                params,
                result: json!({
                "family": "dependabot", "key": key,
                "ecosystem": ecosystem, "package": package, "manifest_path": manifest_path,
                "alert_count": cluster_alerts.len(),
                }),
            });
        }
    }

    let repository = snapshot
        .pointer("/repository/full_name")
        .and_then(Value::as_str)
        .unwrap_or("unknown repository");
    let mut code_alerts = family_alerts(snapshot, "code_scanning");
    code_alerts.sort_by_key(alert_number);
    candidate_count += code_alerts.len();
    for alert in code_alerts {
        let number = alert_number(&alert);
        if number == 0 {
            continue;
        }
        let security_severity = field(&alert, "security_severity").to_ascii_lowercase();
        let rank = severity_rank(&security_severity);
        if rank.is_none_or(|rank| rank < floor) {
            excluded_below_min_severity.push(json!({
                "family": "code_scanning",
                "alert_number": number,
                "rule_id": field(&alert, "rule_id"),
                "path": field(&alert, "path"),
                "security_severity": security_severity,
                "reason": if rank.is_some() {
                    "below_min_severity"
                } else {
                    "missing_or_unrecognized_security_severity"
                },
            }));
            continue;
        }
        let key = digest(&["code-scanning", repository, &number.to_string()]);
        if let Some(DuplicateTaskMatch {
            task_id,
            match_kind,
            evidence,
        }) = find_covering_task(lookup, &code_duplicate_candidate(&key, &alert))
            .map_err(|error| duplicate_lookup_error("code_scanning", &key, &error))?
        {
            skipped_existing.push(json!({
                "family": "code_scanning", "key": key, "task_id": task_id,
                "alert_number": number,
                "match_kind": match_kind, "match_evidence": evidence,
            }));
            continue;
        }
        if pending_tasks.len() >= max_tasks {
            skipped_over_cap.push(json!({
                "family": "code_scanning", "key": key, "alert_number": number,
            }));
            continue;
        }
        let rule = field(&alert, "rule_id");
        let path = field(&alert, "path");
        let rank = rank.unwrap_or(floor);
        let params = TaskAddParams {
            title: code_task_title(&rule, &path),
            description: code_task_description(snapshot, &alert),
            acceptance_criteria: vec![
                format!(
                    "Remediate Code scanning rule `{}` at `{}`{} with a real code or configuration fix; do not suppress, dismiss, or exclude the finding.",
                    display(&rule),
                    display(&path),
                    line_suffix(&alert),
                ),
                "Preserve the intended behavior while removing the data flow or unsafe construct identified in the inline alert evidence.".to_string(),
                "Run the repository's documented validation and security checks and confirm the identified rule no longer reports at the affected location.".to_string(),
            ],
            tags: vec![
                CODE_TAG.to_string(),
                format!("{CODE_KEY_PREFIX}{key}"),
                "security".to_string(),
            ],
            required_tools: Vec::new(),
            crew: system_crew.clone(),
            priority: priority_for_rank(rank),
            complexity: TaskComplexity::Unassessed,
            task_type: Some(TaskType::Bug),
            status: Some(TaskStatus::Backlog),
            system_created: true,
            ..TaskAddParams::default()
        };
        pending_tasks.push(PendingTask {
            params,
            result: json!({
                "family": "code_scanning", "key": key,
                "alert_number": number, "rule_id": rule, "path": path,
            }),
        });
    }

    let mut secret_alerts = family_alerts(snapshot, "secret_scanning");
    secret_alerts.sort_by_key(alert_number);
    candidate_count += secret_alerts.len();
    for alert in secret_alerts {
        let number = alert_number(&alert);
        if number == 0 {
            continue;
        }
        let key = digest(&["secret-scanning", repository, &number.to_string()]);
        if let Some(DuplicateTaskMatch {
            task_id,
            match_kind,
            evidence,
        }) = find_covering_task(lookup, &secret_duplicate_candidate(&key, &alert))
            .map_err(|error| duplicate_lookup_error("secret_scanning", &key, &error))?
        {
            skipped_existing.push(json!({
                "family": "secret_scanning", "key": key, "task_id": task_id,
                "alert_number": number,
                "match_kind": match_kind, "match_evidence": evidence,
            }));
            continue;
        }
        if pending_tasks.len() >= max_tasks {
            skipped_over_cap.push(json!({
                "family": "secret_scanning", "key": key, "alert_number": number,
            }));
            continue;
        }
        let secret_type = field(&alert, "secret_type");
        let params = TaskAddParams {
            title: secret_task_title(&secret_type, number),
            description: secret_task_description(snapshot, &alert),
            acceptance_criteria: vec![
                "Remove the exposed credential from tracked content and repository history wherever applicable, without copying the credential into task updates, logs, commits, or artifacts.".to_string(),
                "Replace the credential use with the repository's approved secret-management or configuration mechanism.".to_string(),
                "Rotate or revoke the exposed credential and record explicit confirmation that rotation or revocation completed, without recording the credential value.".to_string(),
                "Run the repository's documented validation and security checks and verify that no credential material was added to the repository.".to_string(),
            ],
            tags: vec![
                SECRET_TAG.to_string(),
                format!("{SECRET_KEY_PREFIX}{key}"),
                "security".to_string(),
            ],
            required_tools: Vec::new(),
            crew: system_crew.clone(),
            priority: if field(&alert, "validity").eq_ignore_ascii_case("active") {
                TaskPriority::Critical
            } else {
                TaskPriority::High
            },
            complexity: TaskComplexity::Unassessed,
            task_type: Some(TaskType::Bug),
            status: Some(TaskStatus::Backlog),
            system_created: true,
            ..TaskAddParams::default()
        };
        pending_tasks.push(PendingTask {
            params,
            result: json!({
                "family": "secret_scanning", "key": key,
                "alert_number": number, "secret_type": secret_type,
            }),
        });
    }

    // Duplicate lookups for every candidate completed above. Only now may the
    // action begin mutating task state, so a lookup failure cannot leave a
    // partial sweep behind.
    let mut filed = Vec::with_capacity(pending_tasks.len());
    for pending in pending_tasks {
        let task = runtime.add_task(pending.params)?;
        let mut result = pending.result;
        result["task_id"] = json!(task.id);
        filed.push(result);
    }

    let family_outcomes = json!({
        "dependabot": legacy_family_outcome(snapshot),
        "code_scanning": nested_family_outcome(snapshot, "code_scanning"),
        "secret_scanning": nested_family_outcome(snapshot, "secret_scanning"),
    });
    let collection_outcome = snapshot
        .get("collection_status")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if snapshot.get("collected").and_then(Value::as_bool) == Some(true) {
                "partially_collected"
            } else {
                "capability_unavailable"
            }
        });
    let outcome = overall_finding_outcome(&family_outcomes);

    Ok(json!({
        "outcome": outcome,
        "collection_outcome": collection_outcome,
        "family_outcomes": family_outcomes,
        "capability": snapshot.get("capability").cloned().unwrap_or(Value::Null),
        "clusters": candidate_count,
        "filed_count": filed.len(),
        "filed": filed,
        "skipped_existing": skipped_existing,
        "skipped_dependabot_pr": skipped_dependabot_pr,
        "skipped_over_cap": skipped_over_cap,
        "excluded_below_min_severity": excluded_below_min_severity,
        "min_severity": floor_name,
        "skip_when_dependabot_pr_open": skip_pr,
        "max_tasks": max_tasks,
    }))
}

fn family_alerts(snapshot: &Value, family: &str) -> Vec<Value> {
    snapshot
        .get(family)
        .filter(|value| value.get("collected").and_then(Value::as_bool) == Some(true))
        .and_then(|value| value.get("open_alerts"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn legacy_family_outcome(snapshot: &Value) -> Value {
    let collected = snapshot.get("collected").and_then(Value::as_bool) == Some(true);
    json!({
        "collected": collected,
        "collection": if collected {
            if snapshot.get("query_errors").and_then(Value::as_array).is_some_and(|errors| !errors.is_empty()) {
                "partially_collected"
            } else { "fully_collected" }
        } else { "capability_unavailable" },
        "outcome": snapshot.get("outcome_hint").and_then(Value::as_str).unwrap_or("capability_unavailable"),
        "capability": snapshot.get("capability").cloned().unwrap_or(Value::Null),
    })
}

fn nested_family_outcome(snapshot: &Value, family: &str) -> Value {
    let Some(value) = snapshot.get(family) else {
        return json!({
            "collected": false,
            "collection": "capability_unavailable",
            "outcome": "capability_unavailable",
            "capability": null,
        });
    };
    json!({
        "collected": value.get("collected").and_then(Value::as_bool).unwrap_or(false),
        "collection": value.get("collection_status").and_then(Value::as_str).unwrap_or("capability_unavailable"),
        "outcome": value.get("outcome_hint").and_then(Value::as_str).unwrap_or("capability_unavailable"),
        "capability": value.get("capability").cloned().unwrap_or(Value::Null),
    })
}

fn overall_finding_outcome(families: &Value) -> &'static str {
    let outcomes = ["dependabot", "code_scanning", "secret_scanning"].map(|family| {
        families
            .get(family)
            .and_then(|value| value.get("outcome"))
            .and_then(Value::as_str)
            .unwrap_or("capability_unavailable")
    });
    if outcomes.contains(&"open_alerts") {
        "open_alerts"
    } else if outcomes.contains(&"no_open_alerts") {
        "no_open_alerts"
    } else {
        "capability_unavailable"
    }
}

fn dependabot_task_title(package: &str, manifest_path: &str) -> String {
    let budget = 120usize.saturating_sub(DEPENDABOT_TITLE_PREFIX.chars().count());
    let body = format!("Update {package} in {manifest_path}");
    format!("{DEPENDABOT_TITLE_PREFIX}{}", truncate_chars(&body, budget))
}

fn code_task_title(rule: &str, path: &str) -> String {
    let budget = 120usize.saturating_sub(CODE_TITLE_PREFIX.chars().count());
    let body = format!("Fix {} in {}", display(rule), display(path));
    format!("{CODE_TITLE_PREFIX}{}", truncate_chars(&body, budget))
}

fn secret_task_title(secret_type: &str, number: u64) -> String {
    let budget = 120usize.saturating_sub(SECRET_TITLE_PREFIX.chars().count());
    let body = format!(
        "Rotate exposed {} from alert #{number}",
        display(secret_type)
    );
    format!("{SECRET_TITLE_PREFIX}{}", truncate_chars(&body, budget))
}

fn dependabot_task_description(
    snapshot: &Value,
    ecosystem: &str,
    package: &str,
    manifest_path: &str,
    alerts: &[Value],
) -> String {
    let mut patched = alerts
        .iter()
        .map(|alert| field(alert, "first_patched_version"))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    patched.sort();
    patched.dedup();
    let patched = if patched.is_empty() {
        "not published".to_string()
    } else {
        patched.join(", ")
    };
    let repository = repository_name(snapshot);
    let mut out = format!(
        "This task was filed from a bounded, redacted Dependabot snapshot collected on the host before the task existed. The implementing agent does not need GitHub credentials.\n\n## Dependency bump\n\n- Repository: `{repository}`\n- Ecosystem: `{ecosystem}`\n- Package: `{package}`\n- Manifest path: `{manifest_path}`\n- First patched version: `{patched}`\n\n## Open alerts\n\n"
    );
    for alert in alerts {
        let ghsa = field(alert, "ghsa_id");
        let cve = field(alert, "cve_id");
        let identifier = match (ghsa.is_empty(), cve.is_empty()) {
            (false, false) => format!("{ghsa} / {cve}"),
            (false, true) => ghsa,
            (true, false) => cve,
            (true, true) => format!("alert #{}", field(alert, "number")),
        };
        out.push_str(&format!(
            "- **{}** — severity `{}`; {}; vulnerable range `{}`; first patched version `{}`; {}\n",
            identifier,
            display(&field(alert, "severity")),
            display(&field(alert, "summary")),
            display(&field(alert, "vulnerable_version_range")),
            display(&field(alert, "first_patched_version")),
            display(&field(alert, "html_url")),
        ));
    }
    append_collection_bounds(&mut out, snapshot.get("truncation"));
    out.push_str("\nUpdate the dependency and regenerate the lockfile through the repository's normal package-manager workflow. Verify that the manifest and lockfile agree and run the documented pre-handoff checks.\n");
    out
}

fn code_task_description(snapshot: &Value, alert: &Value) -> String {
    let mut out = format!(
        "This task was filed from bounded Code scanning evidence collected on the engine-private host boundary. It does not require agent-side GitHub access.\n\n## Alert evidence\n\n- Repository: `{}`\n- Alert: `#{}`\n- Rule: `{}` ({})\n- Security severity: `{}`\n- Tool: `{}` (version `{}`, guid `{}`)\n- Message: {}\n- Ref: `{}`\n- Commit: `{}`\n- Location: `{}`{}\n- Created: `{}`\n- Updated: `{}`\n- Alert URL: {}\n",
        repository_name(snapshot),
        field(alert, "number"),
        display(&field(alert, "rule_id")),
        display(&field(alert, "rule_name")),
        display(&field(alert, "security_severity")),
        display(&field(alert, "tool_name")),
        display(&field(alert, "tool_version")),
        display(&field(alert, "tool_guid")),
        display(&field(alert, "message")),
        display(&field(alert, "ref")),
        display(&field(alert, "commit_sha")),
        display(&field(alert, "path")),
        line_suffix(alert),
        display(&field(alert, "created_at")),
        display(&field(alert, "updated_at")),
        display(&field(alert, "html_url")),
    );
    append_collection_bounds(&mut out, snapshot.pointer("/code_scanning/truncation"));
    out.push_str("\nRemediate the identified rule at the affected location without suppressing or dismissing the finding, then run the repository's normal validation.\n");
    out
}

fn secret_task_description(snapshot: &Value, alert: &Value) -> String {
    let mut out = format!(
        "This task was filed from a secret-scanning projection that structurally excluded the credential value on the engine-private host boundary. Do not attempt to recover or record the credential.\n\n## Non-secret alert evidence\n\n- Repository: `{}`\n- Alert: `#{}`\n- Secret type: `{}` ({})\n- Validity: `{}`\n- Publicly leaked: `{}`\n- Multi-repository: `{}`\n- Created: `{}`\n- Updated: `{}`\n- Alert URL: {}\n\n## Locations\n\n",
        repository_name(snapshot),
        field(alert, "number"),
        display(&field(alert, "secret_type")),
        display(&field(alert, "secret_type_display_name")),
        display(&field(alert, "validity")),
        display(&field(alert, "publicly_leaked")),
        display(&field(alert, "multi_repo")),
        display(&field(alert, "created_at")),
        display(&field(alert, "updated_at")),
        display(&field(alert, "html_url")),
    );
    let locations = alert
        .get("locations")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if locations.is_empty() {
        out.push_str("- No non-secret location metadata was available; use the alert URL through an authorized human workflow.\n");
    } else {
        for location in locations {
            out.push_str(&format!(
                "- Type `{}`; path `{}`{}; commit `{}`; {}\n",
                display(&field(location, "type")),
                display(&field(location, "path")),
                line_suffix(location),
                display(&field(location, "commit_sha")),
                display(&location_url(location)),
            ));
        }
    }
    if alert
        .get("locations_at_cap")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        out.push_str(
            "- Location collection reached its configured cap; further locations may exist.\n",
        );
    }
    append_collection_bounds(&mut out, snapshot.pointer("/secret_scanning/truncation"));
    out.push_str("\nClean the repository and its history where applicable, migrate use to the approved secret mechanism, and rotate or revoke the credential. Confirm rotation or revocation without recording the credential value.\n");
    out
}

fn append_collection_bounds(out: &mut String, truncation: Option<&Value>) {
    if let Some(truncation) = truncation {
        out.push_str("\n## Collection bounds\n\n");
        out.push_str(&format!(
            "```json\n{}\n```\n",
            serde_json::to_string_pretty(truncation).unwrap_or_default()
        ));
    }
}

fn location_url(location: &Value) -> String {
    for key in [
        "commit_url",
        "blob_url",
        "issue_url",
        "pull_request_url",
        "discussion_url",
    ] {
        let value = field(location, key);
        if !value.is_empty() {
            return value;
        }
    }
    "no location URL".to_string()
}

fn line_suffix(value: &Value) -> String {
    let start = field(value, "start_line");
    let end = field(value, "end_line");
    match (start.is_empty(), end.is_empty(), start == end) {
        (true, _, _) => String::new(),
        (false, true, _) | (false, false, true) => format!(" line {start}"),
        (false, false, false) => format!(" lines {start}-{end}"),
    }
}

fn repository_name(snapshot: &Value) -> &str {
    snapshot
        .pointer("/repository/full_name")
        .and_then(Value::as_str)
        .unwrap_or("unknown repository")
}

/// Match a candidate open Dependabot PR against an alert cluster using
/// identifiers the PR actually asserts — the bumped package parsed from its
/// title, and the package/ecosystem encoded in its `dependabot/<ecosystem>/…`
/// head ref — rather than a free-text substring search over the title and
/// body. A free-text search matched unrelated packages that merely appeared
/// in another package's changelog prose.
fn pull_request_bumps_package(pull_request: &Value, ecosystem: &str, package: &str) -> bool {
    let package = package.to_ascii_lowercase();
    if dependabot_title_package(&field(pull_request, "title"))
        .is_some_and(|title_package| title_package == package)
    {
        return true;
    }
    dependabot_branch_names_package(&field(pull_request, "head_branch"), ecosystem, &package)
}

/// Parse the package a Dependabot PR title bumps, e.g. `Bump serde from 1.0.0
/// to 1.0.1`. Grouped-update titles (`Bump the aws-sdk group with 3 updates`)
/// name no single package and intentionally yield `None`.
fn dependabot_title_package(title: &str) -> Option<String> {
    let mut tokens = title.split_whitespace();
    if !tokens.next()?.eq_ignore_ascii_case("Bump") {
        return None;
    }
    let package = tokens.next()?;
    if package.eq_ignore_ascii_case("the") {
        return None;
    }
    let next = tokens.next()?;
    if next.eq_ignore_ascii_case("from") || next.eq_ignore_ascii_case("to") {
        Some(package.to_ascii_lowercase())
    } else {
        None
    }
}

/// Dependabot head refs follow `dependabot/<ecosystem>/<manifest path…>/<package>-<version>`.
/// Require the ecosystem segment to agree and the final path segment to name
/// the package, so a package whose name is a substring of a sibling
/// package's branch (e.g. `time` inside `runtime`) cannot match.
fn dependabot_branch_names_package(head_branch: &str, ecosystem: &str, package: &str) -> bool {
    let mut segments = head_branch.split('/');
    if segments.next() != Some("dependabot") {
        return false;
    }
    let Some(branch_ecosystem) = segments.next() else {
        return false;
    };
    if !branch_ecosystem.eq_ignore_ascii_case(ecosystem) {
        return false;
    }
    let Some(last) = segments.next_back() else {
        return false;
    };
    let last = last.to_ascii_lowercase();
    last == package || last.starts_with(&format!("{package}-"))
}

fn severity_rank(value: &str) -> Option<u8> {
    match value {
        "low" | "note" | "warning" => Some(1),
        "moderate" | "medium" | "error" => Some(2),
        "high" => Some(3),
        "critical" => Some(4),
        _ => None,
    }
}

fn priority_for_rank(rank: u8) -> TaskPriority {
    match rank {
        4.. => TaskPriority::Critical,
        3 => TaskPriority::High,
        2 => TaskPriority::Medium,
        _ => TaskPriority::Low,
    }
}

fn digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
        .chars()
        .take(KEY_LEN)
        .collect()
}

fn bounded_u64(input: &Value, key: &str, default: u64, max: u64) -> Result<u64, OrbitError> {
    let Some(value) = input.get(key).filter(|value| !value.is_null()) else {
        return Ok(default);
    };
    let raw = match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
    .ok_or_else(|| OrbitError::InvalidInput(format!("input.{key} must be a positive integer")))?;
    if raw == 0 {
        return Err(OrbitError::InvalidInput(format!(
            "input.{key} must be greater than zero"
        )));
    }
    Ok(raw.min(max))
}

fn alert_number(value: &Value) -> u64 {
    value.get("number").and_then(Value::as_u64).unwrap_or(0)
}

fn field(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(value)) => value.to_string(),
        _ => String::new(),
    }
}

fn display(value: &str) -> &str {
    if value.is_empty() { "unknown" } else { value }
}

fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        value.chars().take(max).collect::<String>() + "…"
    }
}
