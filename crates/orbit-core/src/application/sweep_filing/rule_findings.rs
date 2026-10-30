//! Turn one rule-results document (the `ux-rules.json` schema) into tasks.
//!
//! Generic by construction: this filer reads rule ids, targets, statuses,
//! severities, measurements, evidence paths and fingerprints, and knows
//! nothing about any surface that produced them. Any workspace script that
//! writes the schema can feed it. One open task owns one fingerprint,
//! recorded in the task's `rule_fingerprint` field at filing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::task::{
    Task, TaskArtifact, TaskComplexity, TaskPriority, TaskStatus, TaskType,
    media_type_for_artifact_path,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::sweep_filing::{bounded_u64, is_open_ci_owner_status, truncate_chars};
use crate::application::task::{TaskAddParams, TaskUpdateParams};

const SUPPORTED_SCHEMA_VERSION: u64 = 1;
const DEFAULT_MAX_NEW_TASKS: u64 = 10;
const MAX_NEW_TASKS: u64 = 50;
const DEFAULT_RUNNER: &str = "scripts/ux-rules/run.mjs";
/// Evidence larger than this is named in the task, not attached.
const MAX_EVIDENCE_BYTES: u64 = 1024 * 1024;

fn severity_rank(severity: &str) -> u8 {
    match severity.to_ascii_lowercase().as_str() {
        "critical" => 5,
        "high" => 4,
        "medium" => 3,
        "low" => 2,
        "nit" => 1,
        _ => 0,
    }
}

fn priority_for_rank(rank: u8) -> TaskPriority {
    match rank {
        5 => TaskPriority::Critical,
        4 => TaskPriority::High,
        3 => TaskPriority::Medium,
        _ => TaskPriority::Low,
    }
}

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// One config value by dotted key from the admitted effective config.
fn config_value(runtime: &OrbitRuntime, key: &str) -> Option<Value> {
    let config = orbit_config::load_effective_config(&orbit_config::ConfigRoots::new(
        runtime.global_root(),
        runtime.shared_root(),
    ))
    .ok()?;
    config
        .values()
        .iter()
        .find(|entry| entry.key == key)
        .map(|entry| entry.value.clone())
}

struct FilerSettings {
    max_new_tasks: usize,
    file_at_rank: u8,
    file_at: String,
    runner: String,
    disabled: Vec<String>,
}

fn filer_settings(runtime: &OrbitRuntime, input: &Value) -> Result<FilerSettings, OrbitError> {
    let max_new_tasks = if input
        .get("max_new_tasks")
        .is_some_and(|value| !value.is_null())
    {
        bounded_u64(input, "max_new_tasks", DEFAULT_MAX_NEW_TASKS, MAX_NEW_TASKS)? as usize
    } else {
        config_value(runtime, "ux_rules.max_new_tasks")
            .and_then(|value| value.as_u64())
            .unwrap_or(DEFAULT_MAX_NEW_TASKS)
            .min(MAX_NEW_TASKS) as usize
    };
    let file_at = match input.get("file_at") {
        Some(Value::String(value)) if !value.trim().is_empty() => value.trim().to_ascii_lowercase(),
        Some(Value::String(_)) | Some(Value::Null) | None => {
            config_value(runtime, "ux_rules.file_at")
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| "low".to_string())
        }
        _ => {
            return Err(OrbitError::InvalidInput(
                "input.file_at must be one of critical, high, medium, low, or nit".to_string(),
            ));
        }
    };
    let file_at_rank = severity_rank(&file_at);
    if file_at_rank == 0 {
        return Err(OrbitError::InvalidInput(format!(
            "ux_rules.file_at '{file_at}' must be one of critical, high, medium, low, or nit"
        )));
    }
    let runner = match input.get("runner") {
        Some(Value::String(value)) if !value.trim().is_empty() => value.trim().to_string(),
        _ => config_value(runtime, "ux_rules.runner")
            .and_then(|value| value.as_str().map(str::to_string))
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_RUNNER.to_string()),
    };
    let mut disabled = string_list(config_value(runtime, "ux_rules.disabled").as_ref());
    disabled.extend(string_list(input.get("disabled")));
    Ok(FilerSettings {
        max_new_tasks,
        file_at_rank,
        file_at,
        runner,
        disabled,
    })
}

/// The results document and the directory its evidence paths resolve against.
fn load_results(
    runtime: &OrbitRuntime,
    input: &Value,
) -> Result<(Value, Option<PathBuf>), OrbitError> {
    if let Some(results) = input.get("results").filter(|value| !value.is_null()) {
        if !results.is_object() {
            return Err(OrbitError::InvalidInput(
                "input.results must be a rule-results object".to_string(),
            ));
        }
        let evidence_dir = input
            .get("evidence_dir")
            .and_then(Value::as_str)
            .map(|dir| resolve_under_repo(runtime, dir));
        return Ok((results.clone(), evidence_dir));
    }
    let Some(path) = input.get("results_file").and_then(Value::as_str) else {
        return Err(OrbitError::InvalidInput(
            "file_rule_findings requires `results` (a rule-results object) or `results_file` (its path)"
                .to_string(),
        ));
    };
    let full = resolve_under_repo(runtime, path);
    let text = std::fs::read_to_string(&full).map_err(|error| {
        OrbitError::InvalidInput(format!("read rule results {}: {error}", full.display()))
    })?;
    let results: Value = serde_json::from_str(&text).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "rule results {} are not one JSON document: {error}",
            full.display()
        ))
    })?;
    let evidence_dir = full.parent().map(Path::to_path_buf);
    Ok((results, evidence_dir))
}

fn resolve_under_repo(runtime: &OrbitRuntime, path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        runtime.paths().repo_root.join(candidate)
    }
}

fn target_label(target: &Value) -> String {
    if let Some(id) = target.get("id").and_then(Value::as_str) {
        return id.to_string();
    }
    if let Some(view) = target.get("view").and_then(Value::as_str) {
        return view.to_string();
    }
    truncate_chars(&target.to_string(), 80)
}

fn reproduce_command(runner: &str, surface: &str, rule: &str, target: &Value) -> String {
    let launcher = if runner.ends_with(".mjs") || runner.ends_with(".js") {
        format!("node {runner}")
    } else {
        runner.to_string()
    };
    let mut command = launcher;
    if !surface.is_empty() {
        command.push_str(&format!(" --surface {surface}"));
    }
    command.push_str(&format!(" --rule {rule}"));
    command.push_str(" --out <scratch-dir>");
    format!("{command}\n# target: {}", target_label(target))
}

/// `file:` selectors for declared source paths that exist in the checkout.
fn declared_context_files(runtime: &OrbitRuntime, result: &Value) -> Vec<String> {
    let mut declared = string_list(result.get("context_files"));
    declared.extend(string_list(result.get("source_paths")));
    declared
        .into_iter()
        .filter_map(|raw| {
            let path = raw.strip_prefix("file:").unwrap_or(&raw);
            runtime
                .paths()
                .repo_root
                .join(path)
                .exists()
                .then(|| format!("file:{path}"))
        })
        .collect()
}

fn result_tags(surface: &str, rule: &str, result: &Value, error_task: bool) -> Vec<String> {
    let mut tags = vec![
        "ux-rules".to_string(),
        format!("ux-rule:{}", rule.to_ascii_lowercase()),
    ];
    if error_task {
        tags.push("ux-rules-error".to_string());
    }
    if !surface.is_empty() {
        tags.push(surface.to_ascii_lowercase());
    }
    tags.extend(string_list(result.get("tags")));
    tags
}

fn fail_marker(fingerprint: &str) -> String {
    format!("<!-- ux-rules:fail {fingerprint} -->")
}

fn pass_marker(fingerprint: &str) -> String {
    format!("<!-- ux-rules:pass {fingerprint} -->")
}

/// The last filer marker kind recorded for `fingerprint`, newest comment wins.
fn last_marker_kind(
    comments: &[orbit_types::task::TaskComment],
    fingerprint: &str,
) -> Option<&'static str> {
    let fail = fail_marker(fingerprint);
    let pass = pass_marker(fingerprint);
    for comment in comments.iter().rev() {
        if comment.message.contains(&pass) {
            return Some("pass");
        }
        if comment.message.contains(&fail) {
            return Some("fail");
        }
    }
    None
}

fn evidence_artifacts(evidence_dir: Option<&Path>, result: &Value) -> Vec<TaskArtifact> {
    let Some(dir) = evidence_dir else {
        return Vec::new();
    };
    string_list(result.get("evidence"))
        .into_iter()
        .filter_map(|relative| {
            let path = dir.join(&relative);
            let metadata = std::fs::metadata(&path).ok()?;
            if !metadata.is_file() || metadata.len() > MAX_EVIDENCE_BYTES {
                return None;
            }
            let content = std::fs::read(&path).ok()?;
            Some(TaskArtifact {
                media_type: media_type_for_artifact_path(&relative).to_string(),
                path: relative,
                content,
                created_by: Some("system".to_string()),
            })
        })
        .collect()
}

struct FilingCandidate {
    fingerprint: String,
    rule: String,
    rank: u8,
    severity: String,
    params: TaskAddParams,
    artifacts: Vec<TaskArtifact>,
    error_task: bool,
}

pub(crate) fn file_rule_findings(
    runtime: &OrbitRuntime,
    input: &Value,
) -> Result<Value, OrbitError> {
    let (document, evidence_dir) = load_results(runtime, input)?;
    let version = document
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if version > SUPPORTED_SCHEMA_VERSION {
        return Err(OrbitError::InvalidInput(format!(
            "rule results schema version {version} is newer than supported version {SUPPORTED_SCHEMA_VERSION}"
        )));
    }
    let settings = filer_settings(runtime, input)?;
    let surface = field(&document, "surface").to_string();
    let commit = field(&document, "commit").to_string();
    let results = document
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut counts = BTreeMap::from([
        ("pass", 0u64),
        ("fail", 0u64),
        ("error", 0u64),
        ("skipped", 0u64),
    ]);
    for result in &results {
        let status = field(result, "status");
        if let Some(count) = counts.get_mut(status) {
            *count += 1;
        }
    }

    // Open tasks already owning a fingerprint, read once for the whole run.
    let mut owners: BTreeMap<String, Task> = BTreeMap::new();
    for task in runtime.list_tasks_by_tags(&["ux-rules".to_string()])? {
        if is_open_ci_owner_status(task.status)
            && let Some(fingerprint) = &task.rule_fingerprint
        {
            owners.entry(fingerprint.clone()).or_insert(task);
        }
    }

    let mut candidates: Vec<FilingCandidate> = Vec::new();
    let mut commented: Vec<Value> = Vec::new();
    let mut recorded_only: Vec<Value> = Vec::new();
    let mut skipped_existing: Vec<Value> = Vec::new();

    let mut comment_on = |runtime: &OrbitRuntime,
                          task_id: &str,
                          fingerprint: &str,
                          kind: &str,
                          body: String|
     -> Result<(), OrbitError> {
        runtime.update_task(
            task_id,
            TaskUpdateParams {
                comment: Some(body),
                ..Default::default()
            },
        )?;
        commented.push(json!({
            "task_id": task_id, "fingerprint": fingerprint, "kind": kind,
        }));
        Ok(())
    };

    // Passes first: exactly one comment per fail-to-pass transition, never a
    // status change. A repeated pass while the last marker is already a
    // pass comments nothing.
    for result in &results {
        if field(result, "status") != "pass" {
            continue;
        }
        let fingerprint = field(result, "fingerprint");
        let Some(owner) = owners.get(fingerprint) else {
            continue;
        };
        if settings
            .disabled
            .iter()
            .any(|rule| rule == field(result, "rule"))
        {
            continue;
        }
        let comments = runtime.get_task_comments(&owner.id)?;
        if last_marker_kind(&comments, fingerprint) == Some("pass") {
            continue;
        }
        let measurements = result.get("measurements").cloned().unwrap_or(json!({}));
        comment_on(
            runtime,
            &owner.id,
            fingerprint,
            "pass",
            format!(
                "Rule `{}` passes for fingerprint `{fingerprint}` at commit `{}`.\n\n```json\n{}\n```\n{}",
                field(result, "rule"),
                if commit.is_empty() {
                    "unknown"
                } else {
                    &commit
                },
                measurements,
                pass_marker(fingerprint),
            ),
        )?;
    }

    // Errors group to one candidate per rule, not per target.
    let mut errors_by_rule: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for result in &results {
        if field(result, "status") == "error" {
            errors_by_rule
                .entry(field(result, "rule").to_string())
                .or_default()
                .push(result.clone());
        }
    }
    for (rule, group) in &errors_by_rule {
        if rule.is_empty() || settings.disabled.contains(rule) {
            continue;
        }
        let fingerprint = format!("error|{rule}");
        if let Some(owner) = owners.get(&fingerprint) {
            skipped_existing.push(json!({
                "fingerprint": fingerprint, "task_id": owner.id, "status": "error",
            }));
            continue;
        }
        let rank = group
            .iter()
            .map(|result| severity_rank(field(result, "severity")))
            .max()
            .unwrap_or(3);
        let targets: Vec<String> = group
            .iter()
            .map(|result| {
                let target = result.get("target").cloned().unwrap_or(json!({}));
                format!(
                    "- {} — measurements: {}",
                    target_label(&target),
                    result.get("measurements").cloned().unwrap_or(json!({})),
                )
            })
            .collect();
        let first_target = group[0].get("target").cloned().unwrap_or(json!({}));
        candidates.push(FilingCandidate {
            params: TaskAddParams {
                title: format!("Rule {rule} is broken: it errored instead of measuring"),
                description: format!(
                    "The rule runner reported `error` for rule `{rule}` on {} target(s). An error is a rule defect, never a product finding.\n\n{}\n\nReproduce:\n\n```sh\n{}\n```\n\nFingerprint: `{fingerprint}`",
                    group.len(),
                    targets.join("\n"),
                    reproduce_command(&settings.runner, &surface, rule, &first_target),
                ),
                acceptance_criteria: vec![
                    format!("Running the runner for rule `{rule}` reports pass, fail or skipped per target, never error."),
                ],
                tags: result_tags(&surface, rule, &group[0], true),
                priority: priority_for_rank(rank),
                complexity: TaskComplexity::Low,
                task_type: Some(TaskType::Bug),
                status: Some(TaskStatus::Proposed),
                system_created: true,
                rule_fingerprint: Some(fingerprint.clone()),
                context_files: declared_context_files(runtime, &group[0]),
                ..TaskAddParams::default()
            },
            artifacts: group
                .iter()
                .flat_map(|result| evidence_artifacts(evidence_dir.as_deref(), result))
                .collect(),
            fingerprint,
            rule: rule.clone(),
            rank,
            severity: field(&group[0], "severity").to_string(),
            error_task: true,
        });
    }

    // Failures: file new fingerprints, comment owners on changed numbers.
    for result in &results {
        if field(result, "status") != "fail" {
            continue;
        }
        let rule = field(result, "rule").to_string();
        let fingerprint = field(result, "fingerprint").to_string();
        if rule.is_empty() || fingerprint.is_empty() {
            continue;
        }
        let severity = field(result, "severity").to_ascii_lowercase();
        let rank = severity_rank(&severity);
        if settings.disabled.contains(&rule) {
            recorded_only.push(json!({
                "fingerprint": fingerprint, "rule": rule, "reason": "disabled",
            }));
            continue;
        }
        if rank < settings.file_at_rank {
            recorded_only.push(json!({
                "fingerprint": fingerprint, "rule": rule, "severity": severity,
                "reason": "below_file_at",
            }));
            continue;
        }
        let measurements = result.get("measurements").cloned().unwrap_or(json!({}));
        if let Some(owner) = owners.get(&fingerprint) {
            let rendered = measurements.to_string();
            let comments = runtime.get_task_comments(&owner.id)?;
            let fail_mark = fail_marker(fingerprint.as_str());
            let pass_mark = pass_marker(fingerprint.as_str());
            // Compare against the newest filer comment only: an older value
            // repeating after a different latest value is still a change.
            // A re-fail after a pass always records a transition, otherwise
            // the next pass would see a stale pass marker and stay silent.
            let last_is_pass = last_marker_kind(&comments, fingerprint.as_str()) == Some("pass");
            let latest_recorded = comments.iter().rev().find(|comment| {
                comment.message.contains(&fail_mark) || comment.message.contains(&pass_mark)
            });
            let unchanged = if last_is_pass {
                false
            } else if let Some(latest) = latest_recorded {
                latest.message.contains(&rendered)
            } else {
                owner.description.contains(&rendered)
            };
            if unchanged {
                skipped_existing.push(json!({
                    "fingerprint": fingerprint, "task_id": owner.id, "status": "fail",
                }));
            } else {
                comment_on(
                    runtime,
                    &owner.id,
                    &fingerprint,
                    "measurements_changed",
                    format!(
                        "Rule `{rule}` still fails for fingerprint `{fingerprint}` with changed measurements:\n\n```json\n{measurements}\n```\n{}",
                        fail_marker(&fingerprint),
                    ),
                )?;
            }
            continue;
        }
        let target = result.get("target").cloned().unwrap_or(json!({}));
        let title = {
            let declared = field(result, "title");
            if declared.is_empty() {
                format!("{rule}: {}", target_label(&target))
            } else {
                declared.to_string()
            }
        };
        let evidence = string_list(result.get("evidence"));
        candidates.push(FilingCandidate {
            params: TaskAddParams {
                title,
                description: format!(
                    "Rule `{rule}` failed.\n\n- Target: `{}`\n- Severity: `{severity}`\n- Fingerprint: `{fingerprint}`\n\nMeasurements:\n\n```json\n{measurements}\n```\n\nEvidence: {}\n\nReproduce:\n\n```sh\n{}\n```",
                    target_label(&target),
                    if evidence.is_empty() {
                        "none recorded".to_string()
                    } else {
                        evidence.join(", ")
                    },
                    reproduce_command(&settings.runner, &surface, &rule, &target),
                ),
                acceptance_criteria: vec![
                    format!("Re-running rule `{rule}` for this target reports pass."),
                ],
                tags: result_tags(&surface, &rule, result, false),
                priority: priority_for_rank(rank),
                complexity: TaskComplexity::Unassessed,
                task_type: Some(TaskType::Bug),
                status: Some(TaskStatus::Proposed),
                system_created: true,
                rule_fingerprint: Some(fingerprint.clone()),
                context_files: declared_context_files(runtime, result),
                ..TaskAddParams::default()
            },
            artifacts: evidence_artifacts(evidence_dir.as_deref(), result),
            fingerprint,
            rule,
            rank,
            severity,
            error_task: false,
        });
    }

    // Highest severity first; fingerprint order breaks ties deterministically.
    candidates.sort_by(|a, b| b.rank.cmp(&a.rank).then(a.fingerprint.cmp(&b.fingerprint)));
    let mut filed: Vec<Value> = Vec::new();
    let mut skipped_over_cap: Vec<Value> = Vec::new();
    for candidate in candidates {
        if filed.len() >= settings.max_new_tasks {
            skipped_over_cap.push(json!({
                "fingerprint": candidate.fingerprint, "rule": candidate.rule,
                "severity": candidate.severity,
            }));
            continue;
        }
        let fingerprint = candidate.fingerprint.clone();
        let task = runtime.add_task(candidate.params)?;
        if !candidate.artifacts.is_empty() {
            runtime.update_task(
                &task.id,
                TaskUpdateParams {
                    upsert_artifacts: candidate.artifacts,
                    ..Default::default()
                },
            )?;
        }
        filed.push(json!({
            "task_id": task.id, "fingerprint": fingerprint, "rule": candidate.rule,
            "severity": candidate.severity, "error_task": candidate.error_task,
        }));
    }

    Ok(json!({
        "outcome": "filed",
        "counts": {
            "pass": counts["pass"], "fail": counts["fail"],
            "error": counts["error"], "skipped": counts["skipped"],
        },
        "filed_count": filed.len(),
        "filed": filed,
        "commented": commented,
        "skipped_existing": skipped_existing,
        "skipped_over_cap": skipped_over_cap,
        "recorded_only": recorded_only,
        "max_new_tasks": settings.max_new_tasks,
        "file_at": settings.file_at,
    }))
}
