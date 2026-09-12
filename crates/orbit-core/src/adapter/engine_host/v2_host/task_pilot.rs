//! Deterministic support actions for the task-pilot workflow [ORB-10510].
//!
//! The agent leg only proposes task metadata. These actions own discovery,
//! partitioning and canonical selector validation. Ordinarily the sole write
//! is persisting assessed complexity and replacing `context_files` on the exact
//! tasks prepared for the run, with the assessment audit and replay receipt in
//! the same task-bundle commit. A CI-failure sweep may additionally request
//! explicit admission: after the selectors and every recommendation validate,
//! this boundary promotes only a current, warning-free repair from `proposed`
//! to `backlog`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use orbit_common::fs::selector::{
    anchor_path, canonical_selector, canonical_selector_in_workspace, exists_in_workspace,
};
use orbit_engine::DispatchError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::{TaskComplexity, TaskEnvelopeV2, TaskStatus};
use orbit_types::workflow::JobRunState;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::TaskListFilter;

mod apply;
mod source;
mod validation_tools;

pub(super) use apply::apply;
#[cfg(test)]
pub(super) use apply::inject_concurrent_edit_before_locked_apply;
use source::{GitPathKind, SourceSnapshot, requested_base_branch, resolve_source_snapshot};
use validation_tools::ImplementationLane;

const DEFAULT_MAX_PARTITION_SIZE: usize = 5;
const HARD_MAX_PARTITION_SIZE: usize = 5;
const DEFAULT_MAX_TASKS: usize = 50;
const HARD_MAX_TASKS: usize = 500;
const NO_DIFF_TAGS: [&str; 2] = ["no-diff-needed", "no-diff-expected"];
const NO_DIFF_EXPECTED_TAG: &str = "no-diff-expected";
const AUTO_TASK_TAG_PREFIX: &str = "auto-task:";
const TASK_PILOT_JOB_ID: &str = "task_pilot_pipeline";
/// Cap on individually itemized `excluded` entries in routine discovery output.
/// Automatic-mode exclusions still carry full counts by reason; this bounds
/// only the per-task sample so evidence size stops scaling with terminal
/// workspace history [ORB-11244].
const MAX_EXCLUDED_SAMPLE: usize = 20;
/// Field carrying the deterministic validation-tool feasibility findings, on
/// both a prepared task snapshot and the assessment apply reports for it
/// [ORB-11980].
pub(super) const VALIDATION_TOOL_WARNINGS: &str = "validation_tool_warnings";

pub(super) fn prepare(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let workspace_root = requested_workspace_root(runtime, action, input)?;
    let claim = orbit_automation::consumers::members::claim(runtime, input)
        .map_err(|error| action_failed(action, error.to_string()))?;
    let source = resolve_source_snapshot(runtime, action, input, &workspace_root)?;
    if let Some(claim) = &claim
        && (source.as_ref().map(|s| &s.source_revision) != Some(&claim.member.source.commit)
            || input.get("task_ids") != Some(&json!(claim.member.task_ids)))
    {
        return Err(action_failed(
            action,
            "state-trigger source or task membership changed",
        ));
    }
    let max_partition_size = bounded_usize(
        action,
        input,
        "max_partition_size",
        DEFAULT_MAX_PARTITION_SIZE,
        HARD_MAX_PARTITION_SIZE,
    )?;
    let max_tasks = bounded_usize(
        action,
        input,
        "max_tasks",
        DEFAULT_MAX_TASKS,
        HARD_MAX_TASKS,
    )?;
    let explicit_task_ids = string_array(input, "task_ids", action)?;
    let explicit_mode = !explicit_task_ids.is_empty();
    let active_preparations = active_task_pilot_preparations(runtime, action, &workspace_root)?;

    let (mode, task_ids, mut task_snapshots, excluded) = if explicit_mode {
        let all_tasks = runtime
            .list_tasks()
            .map_err(|error| action_failed(action, format!("list workspace tasks: {error}")))?;
        let by_id = all_tasks
            .iter()
            .map(|task| (task.id.as_str(), task))
            .collect::<BTreeMap<_, _>>();
        let mut seen = BTreeSet::new();
        let selected = explicit_task_ids
            .iter()
            .map(|task_id| {
                if !seen.insert(task_id.as_str()) {
                    return Err(action_failed(
                        action,
                        format!("explicit task_ids contains duplicate {task_id}"),
                    ));
                }
                by_id.get(task_id.as_str()).copied().ok_or_else(|| {
                    action_failed(
                        action,
                        format!("task {task_id} does not exist in the selected workspace"),
                    )
                }).and_then(|task| {
                    if let Some(run_ids) = active_preparations.get(task_id) {
                        Err(action_failed(
                            action,
                            format!(
                                "task {task_id} is already prepared by active task-pilot run(s) {}; inspect or resume that durable run instead of starting duplicate pilot work",
                                run_ids.iter().cloned().collect::<Vec<_>>().join(", ")
                            ),
                        ))
                    } else {
                        Ok(task)
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let task_ids = selected
            .iter()
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        let task_snapshots = selected
            .iter()
            .map(|task| {
                task_snapshot(
                    &task.id,
                    &task.title,
                    task.status,
                    task.complexity,
                    &task.tags,
                    &task.context_files,
                )
            })
            .collect::<Vec<_>>();
        (
            "explicit",
            task_ids,
            task_snapshots,
            ExcludedEvidence::default(),
        )
    } else {
        // Envelope-only metadata (no description/plan/comments/history/artifacts)
        // for every task in the workspace, so routine discovery no longer pays
        // for full-bundle hydration of terminal history it will only exclude.
        let candidates = runtime
            .task_candidates(&TaskListFilter::default(), usize::MAX)
            .map_err(|error| {
                action_failed(action, format!("list workspace task envelopes: {error}"))
            })?;
        let mut envelopes = candidates.items;
        envelopes.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.id.cmp(&right.id))
        });

        let mut selected = Vec::new();
        let mut excluded = ExcludedEvidence::default();
        for envelope in &envelopes {
            let reason = automatic_exclusion_reason(
                envelope.status,
                &envelope.context_files,
                envelope.complexity,
                &envelope.tags,
            )
            .or_else(|| {
                active_preparations
                    .contains_key(&envelope.id)
                    .then_some("active_pilot_prepared")
            });
            match reason {
                Some(reason) => excluded.record(envelope, reason, &active_preparations),
                None => selected.push(envelope),
            }
        }

        let task_ids = selected
            .iter()
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        let task_snapshots = selected
            .iter()
            .map(|task| {
                task_snapshot(
                    &task.id,
                    &task.title,
                    task.status,
                    task.complexity,
                    &task.tags,
                    &task.context_files,
                )
            })
            .collect::<Vec<_>>();
        ("automatic", task_ids, task_snapshots, excluded)
    };

    if task_ids.len() > max_tasks {
        return Err(action_failed(
            action,
            format!(
                "{mode} task-pilot selection contains {} tasks, exceeding max_tasks {max_tasks}",
                task_ids.len()
            ),
        ));
    }

    // One hydration per selected task feeds both the state-automation
    // fingerprint and the validation-tool feasibility check. Discovery above
    // deliberately works from envelopes, which carry no acceptance criteria,
    // and the selection is already bounded by `max_tasks` at this point.
    let lane = ImplementationLane::resolve(runtime);
    for (task_id, snapshot) in task_ids.iter().zip(task_snapshots.iter_mut()) {
        let task = runtime
            .get_task(task_id)
            .map_err(|error| action_failed(action, error.to_string()))?;
        snapshot[VALIDATION_TOOL_WARNINGS] = json!(lane.validation_warnings(&task));

        let Some(source) = &source else {
            continue;
        };
        let fingerprint = orbit_automation::consumers::preparation::fingerprint(
            runtime,
            &task,
            &source.source_revision,
        )
        .map_err(|error| action_failed(action, error.to_string()))?;
        if claim
            .as_ref()
            .is_some_and(|claim| claim.member.fingerprint != fingerprint)
        {
            return Err(action_failed(action, "state-trigger task meaning changed"));
        }
        snapshot["material_fingerprint"] = json!(fingerprint);
    }

    let partitions = task_ids
        .chunks(max_partition_size)
        .enumerate()
        .map(|(partition_index, ids)| {
            json!({
                "partition_index": partition_index,
                "task_ids": ids,
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "state_automation": claim,
        "mode": mode,
        "workspace_path": workspace_root,
        "source": source.as_ref().map(SourceSnapshot::to_json).unwrap_or_else(|| {
            json!({
                "base_branch": requested_base_branch(runtime, input),
                "source_ref": Value::Null,
                "source_revision": Value::Null,
                "fast_forwarded": false,
            })
        }),
        "task_count": task_ids.len(),
        "task_ids": task_ids,
        "tasks": task_snapshots,
        "partition_size": max_partition_size,
        "partition_count": partitions.len(),
        "partitions": partitions,
        "excluded": excluded.sample,
        "excluded_total": excluded.total,
        "excluded_by_reason": excluded.by_reason,
        "excluded_sample_truncated": excluded.total > excluded.sample.len(),
        "excluded_omitted_count": excluded.total.saturating_sub(excluded.sample.len()),
    }))
}

/// Bounded evidence for routine (non-explicit) discovery exclusions: every
/// excluded task is counted by reason, but only a capped sample is itemized
/// so response size stops scaling with terminal workspace history.
#[derive(Default)]
struct ExcludedEvidence {
    sample: Vec<Value>,
    total: usize,
    by_reason: BTreeMap<&'static str, usize>,
}

impl ExcludedEvidence {
    fn record(
        &mut self,
        envelope: &TaskEnvelopeV2,
        reason: &'static str,
        active_preparations: &BTreeMap<String, BTreeSet<String>>,
    ) {
        self.total += 1;
        *self.by_reason.entry(reason).or_default() += 1;
        if self.sample.len() >= MAX_EXCLUDED_SAMPLE {
            return;
        }
        let mut entry = json!({ "task_id": envelope.id, "reason": reason });
        if reason == "active_pilot_prepared"
            && let Some(run_ids) = active_preparations.get(&envelope.id)
            && let Value::Object(fields) = &mut entry
        {
            fields.insert("prepared_by_run_ids".to_string(), json!(run_ids));
        }
        self.sample.push(entry);
    }
}

fn active_task_pilot_preparations(
    runtime: &OrbitRuntime,
    action: &str,
    workspace_root: &Path,
) -> Result<BTreeMap<String, BTreeSet<String>>, DispatchError> {
    let mut prepared_by_task = BTreeMap::<String, BTreeSet<String>>::new();
    for state in [
        JobRunState::Pending,
        JobRunState::Running,
        JobRunState::Retrying,
    ] {
        let runs = runtime
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some(TASK_PILOT_JOB_ID.to_string()),
                state: Some(state),
                terminal_only: false,
                created_since: None,
                limit: None,
                ..Default::default()
            })
            .map_err(|error| {
                action_failed(action, format!("list active task-pilot runs: {error}"))
            })?;
        for run in runs {
            let run = runtime.show_job_run(&run.run_id).map_err(|error| {
                action_failed(
                    action,
                    format!("reconcile active task-pilot run {}: {error}", run.run_id),
                )
            })?;
            if run.state.is_terminal() {
                continue;
            }
            let Some(state) = runtime.read_run_state(&run.run_id).map_err(|error| {
                action_failed(
                    action,
                    format!("read active task-pilot run {} state: {error}", run.run_id),
                )
            })?
            else {
                continue;
            };
            let Some(task_ids) = state.step_outputs.iter().find_map(|(step_index, output)| {
                (state.step_states.get(step_index) == Some(&JobRunState::Success))
                    .then(|| prepared_task_ids(output, workspace_root))
                    .flatten()
            }) else {
                continue;
            };
            for task_id in task_ids {
                prepared_by_task
                    .entry(task_id)
                    .or_default()
                    .insert(run.run_id.clone());
            }
        }
    }
    Ok(prepared_by_task)
}

fn prepared_task_ids(output: &Value, workspace_root: &Path) -> Option<Vec<String>> {
    let object = output.as_object()?;
    let prepared_workspace = object.get("workspace_path")?.as_str()?;
    if Path::new(prepared_workspace) != workspace_root
        || !object.get("partitions")?.is_array()
        || !object.get("tasks")?.is_array()
        || !matches!(object.get("mode")?.as_str(), Some("automatic" | "explicit"))
    {
        return None;
    }
    object
        .get("task_ids")?
        .as_array()?
        .iter()
        .map(|task_id| task_id.as_str().map(ToOwned::to_owned))
        .collect()
}

fn automatic_exclusion_reason(
    status: TaskStatus,
    context_files: &[String],
    complexity: Option<TaskComplexity>,
    tags: &[String],
) -> Option<&'static str> {
    if !matches!(status, TaskStatus::Proposed | TaskStatus::Backlog) {
        return Some("status_not_eligible");
    }
    if !context_files.is_empty() && complexity.is_some_and(TaskComplexity::is_assessed) {
        return Some("context_files_not_empty");
    }
    if tags.iter().any(|tag| NO_DIFF_TAGS.contains(&tag.as_str()))
        && !is_no_diff_expected_auto_task(tags)
    {
        return Some("no_diff_task");
    }
    None
}

/// Automatically minted no-diff work still needs a complexity assessment, but
/// it cannot honestly produce modification selectors. The mint provenance tag
/// distinguishes it from an ordinary task that merely carries a no-diff tag.
fn is_no_diff_expected_auto_task(tags: &[String]) -> bool {
    tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG)
        && tags.iter().any(|tag| tag.starts_with(AUTO_TASK_TAG_PREFIX))
}

fn task_snapshot(
    id: &str,
    title: &str,
    status: TaskStatus,
    complexity: Option<TaskComplexity>,
    tags: &[String],
    context_files: &[String],
) -> Value {
    json!({
        "task_id": id,
        "title": title,
        "status": status,
        "complexity": complexity,
        "tags": tags,
        "context_files_before": context_files,
    })
}

struct ValidatedSelectors {
    values: Vec<String>,
    normalizations: Vec<Value>,
}

fn validate_after_selectors(
    action: &str,
    task_id: &str,
    disposition: &str,
    assessment: &Value,
    selectors: &[String],
    workspace_root: &Path,
    source: Option<&SourceSnapshot>,
) -> Result<ValidatedSelectors, DispatchError> {
    if selectors.is_empty() {
        if !matches!(disposition, "verified_no_diff" | "host_operational") {
            return Err(action_failed(
                action,
                format!(
                    "task {task_id} may keep empty context_files only with verified_no_diff or host_operational disposition"
                ),
            ));
        }
        required_string(assessment, "evidence", action)?;
        return Ok(ValidatedSelectors {
            values: Vec::new(),
            normalizations: Vec::new(),
        });
    }
    if disposition != "selectors" {
        return Err(action_failed(
            action,
            format!(
                "task {task_id} has non-empty context_files_after but disposition is {disposition}"
            ),
        ));
    }

    let mut seen = BTreeSet::new();
    let mut values = Vec::with_capacity(selectors.len());
    let mut normalizations = Vec::new();
    for selector in selectors {
        let trimmed = selector.trim();
        let has_kind = matches!(
            trimmed.split_once(':').map(|(kind, _)| kind),
            Some("file" | "dir" | "symbol")
        );
        let candidate = if has_kind {
            trimmed.to_string()
        } else {
            if trimmed.contains(':') {
                return Err(action_failed(
                    action,
                    format!(
                        "task {task_id} bare selector {selector:?} is ambiguous because it contains ':'"
                    ),
                ));
            }
            let source = source.ok_or_else(|| {
                action_failed(
                    action,
                    format!(
                        "task {task_id} selector {selector:?} must use file:, dir:, or symbol: when no pinned source is available"
                    ),
                )
            })?;
            let path_candidate =
                canonical_selector(&format!("file:{trimmed}")).map_err(|error| {
                    action_failed(
                        action,
                        format!("task {task_id} bare selector {selector:?} is invalid: {error}"),
                    )
                })?;
            let anchor = anchor_path(&path_candidate).map_err(|error| {
                action_failed(
                    action,
                    format!("task {task_id} bare selector {selector:?} is invalid: {error}"),
                )
            })?;
            let kind = source.path_kind(action, workspace_root, &anchor)?;
            let normalized = match kind {
                GitPathKind::Blob => path_candidate,
                GitPathKind::Tree => canonical_selector(&format!("dir:{trimmed}"))
                    .map_err(|error| action_failed(action, error.to_string()))?,
                GitPathKind::Missing => {
                    return Err(action_failed(
                        action,
                        format!(
                            "task {task_id} bare selector {selector:?} does not resolve at pinned source revision {}",
                            source.source_revision
                        ),
                    ));
                }
                GitPathKind::Other => {
                    return Err(action_failed(
                        action,
                        format!(
                            "task {task_id} bare selector {selector:?} has an unsupported or ambiguous kind at pinned source revision {}",
                            source.source_revision
                        ),
                    ));
                }
            };
            normalizations.push(json!({
                "original": selector,
                "normalized": normalized,
            }));
            normalized
        };
        // A dirty primary may replace an anchor with a symlink or a different
        // kind. Only syntax comes from this parser; pinned Git objects own
        // containment and target validation when source identity is present.
        let canonical = if source.is_some() {
            canonical_selector(&candidate)
        } else {
            canonical_selector_in_workspace(&candidate, workspace_root)
        }
        .map_err(|error| {
            action_failed(
                action,
                format!("task {task_id} selector {selector:?} is invalid: {error}"),
            )
        })?;
        if has_kind && canonical != trimmed {
            return Err(action_failed(
                action,
                format!(
                    "task {task_id} selector {selector:?} is not canonical; expected {canonical:?}"
                ),
            ));
        }
        if let Some(source) = source {
            validate_selector_at_source(action, task_id, &canonical, workspace_root, source)?;
        } else {
            if !exists_in_workspace(&canonical, workspace_root) {
                return Err(action_failed(
                    action,
                    format!(
                        "task {task_id} selector {selector:?} does not resolve to an existing in-workspace target"
                    ),
                ));
            }
            validate_selector_target_kind(action, task_id, &canonical, workspace_root)?;
        }
        if !seen.insert(canonical) {
            return Err(action_failed(
                action,
                format!("task {task_id} repeats selector {selector:?}"),
            ));
        }
        values.push(candidate);
    }
    Ok(ValidatedSelectors {
        values,
        normalizations,
    })
}

/// Whether a validated assessment leaves the task ready for the state
/// automation to promote it on its own. Actionable selectors are necessary but
/// not sufficient: any finding that names other work, or an action outside
/// what this repository owns — a duplicate, a repair that already landed, or
/// an operator-reserved release action — keeps that decision with a human
/// [ORB-11517]. A validation criterion the implementation lane cannot satisfy
/// does the same, because promoting it admits work whose acceptance check is
/// already known to be unreachable [ORB-11980].
pub(super) fn member_ready(assessment: &Value) -> bool {
    // Unlike the agent's own findings below, this field is injected by the
    // deterministic apply boundary, so an assessment that predates the
    // injection reads as "no finding" rather than as "not ready".
    let validation_tools_feasible = assessment
        .get(VALIDATION_TOOL_WARNINGS)
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty);

    validation_tools_feasible
        && matches!(
            assessment
                .get("recommended_complexity")
                .and_then(Value::as_str),
            Some("low" | "medium" | "hard")
        )
        && assessment["disposition"] == "selectors"
        && [
            "blocked_by",
            "adr_conflicts",
            "utility_warnings",
            "surface_warnings",
        ]
        .iter()
        .all(|field| {
            assessment
                .get(field)
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
        })
        && ["duplicate_of", "already_landed", "release_action_required"]
            .iter()
            .all(|field| assessment.get(field).is_none_or(Value::is_null))
}

fn validate_recommendations(
    action: &str,
    task_id: &str,
    assessment: &Value,
) -> Result<TaskComplexity, DispatchError> {
    required_string(assessment, "recommended_crew", action)?;
    let complexity = required_string(assessment, "recommended_complexity", action)?;
    let complexity = complexity.parse::<TaskComplexity>().map_err(|_| {
        action_failed(
            action,
            format!(
                "task {task_id} recommended_complexity must be low, medium, hard, or unassessed"
            ),
        )
    })?;
    required_string(assessment, "assessment_rationale", action)?;
    required_string(assessment, "validation_approach", action)?;
    let confidence = required_string(assessment, "confidence", action)?;
    if !matches!(confidence, "high" | "medium" | "low") {
        return Err(action_failed(
            action,
            format!("task {task_id} confidence must be high, medium, or low"),
        ));
    }
    let evidence_gaps = required_string_array(assessment, "evidence_gaps", action)?;
    required_string_array(assessment, "reassessment_triggers", action)?;
    if complexity == TaskComplexity::Unassessed && evidence_gaps.is_empty() {
        return Err(action_failed(
            action,
            format!("task {task_id} unassessed complexity requires actionable evidence_gaps"),
        ));
    }
    if complexity.is_assessed() && !evidence_gaps.is_empty() && confidence == "high" {
        return Err(action_failed(
            action,
            format!("task {task_id} with high confidence must not have evidence_gaps"),
        ));
    }
    for field in [
        "blocked_by",
        "adr_conflicts",
        "utility_warnings",
        "surface_warnings",
    ] {
        string_array_value(
            assessment.get(field).ok_or_else(|| {
                action_failed(action, format!("task {task_id} is missing {field}"))
            })?,
            field,
            action,
        )?;
    }
    for field in ["duplicate_of", "already_landed"] {
        if assessment.get(field).is_none() {
            return Err(action_failed(
                action,
                format!("task {task_id} is missing {field} recommendation"),
            ));
        }
    }
    validate_optional_finding(
        action,
        task_id,
        assessment,
        "duplicate_of",
        &["task_id", "evidence"],
    )?;
    validate_optional_finding(action, task_id, assessment, "already_landed", &["evidence"])?;
    validate_optional_finding(
        action,
        task_id,
        assessment,
        "release_action_required",
        &["action", "evidence"],
    )?;
    Ok(complexity)
}

fn validate_optional_finding(
    action: &str,
    task_id: &str,
    assessment: &Value,
    field: &str,
    required_fields: &[&str],
) -> Result<(), DispatchError> {
    let Some(value) = assessment.get(field) else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }
    let object = value.as_object().ok_or_else(|| {
        action_failed(
            action,
            format!("task {task_id} {field} must be an object or null"),
        )
    })?;
    for required in required_fields {
        object
            .get(*required)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                action_failed(
                    action,
                    format!("task {task_id} {field}.{required} must be a non-empty string"),
                )
            })?;
    }
    Ok(())
}

fn validate_selector_at_source(
    action: &str,
    task_id: &str,
    selector: &str,
    workspace_root: &Path,
    source: &SourceSnapshot,
) -> Result<(), DispatchError> {
    let anchor = anchor_path(selector).map_err(|error| {
        action_failed(
            action,
            format!("task {task_id} selector {selector:?} has no filesystem anchor: {error}"),
        )
    })?;
    let kind = source.path_kind(action, workspace_root, &anchor)?;
    let expected_dir = selector.starts_with("dir:");
    match (kind, expected_dir) {
        (GitPathKind::Tree, true) | (GitPathKind::Blob, false) => Ok(()),
        (GitPathKind::Missing, _) => Err(action_failed(
            action,
            format!(
                "task {task_id} selector {selector:?} does not resolve to an existing in-workspace target at source revision {} ({})",
                source.source_revision, source.source_ref
            ),
        )),
        _ => Err(action_failed(
            action,
            format!(
                "task {task_id} selector {selector:?} does not match the target's file/directory kind at source revision {} ({})",
                source.source_revision, source.source_ref
            ),
        )),
    }
}

fn validate_selector_target_kind(
    action: &str,
    task_id: &str,
    selector: &str,
    workspace_root: &Path,
) -> Result<(), DispatchError> {
    let anchor = anchor_path(selector).map_err(|error| {
        action_failed(
            action,
            format!("task {task_id} selector {selector:?} has no filesystem anchor: {error}"),
        )
    })?;
    let resolved = workspace_root.join(anchor);
    let correct_kind = if selector.starts_with("dir:") {
        resolved.is_dir()
    } else {
        resolved.is_file()
    };
    if correct_kind {
        Ok(())
    } else {
        Err(action_failed(
            action,
            format!(
                "task {task_id} selector {selector:?} does not match the target's file/directory kind"
            ),
        ))
    }
}

fn requested_workspace_root(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<PathBuf, DispatchError> {
    let runtime_root = runtime.paths().repo_root.canonicalize().map_err(|error| {
        action_failed(action, format!("canonicalize runtime workspace: {error}"))
    })?;
    let requested = input
        .get("workspace_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| runtime_root.clone());
    let requested = if requested.is_absolute() {
        requested
    } else {
        runtime_root.join(requested)
    };
    let requested = requested.canonicalize().map_err(|error| {
        action_failed(
            action,
            format!(
                "canonicalize requested workspace {}: {error}",
                requested.display()
            ),
        )
    })?;
    if requested != runtime_root {
        return Err(action_failed(
            action,
            format!(
                "requested workspace {} does not match active workspace {}",
                requested.display(),
                runtime_root.display()
            ),
        ));
    }
    Ok(runtime_root)
}

fn bounded_usize(
    action: &str,
    input: &Value,
    field: &str,
    default: usize,
    max: usize,
) -> Result<usize, DispatchError> {
    let value = input
        .get(field)
        .and_then(Value::as_u64)
        .unwrap_or(default as u64);
    if value == 0 || value > max as u64 {
        return Err(action_failed(
            action,
            format!("`{field}` must be between 1 and {max}"),
        ));
    }
    Ok(value as usize)
}

fn string_array(input: &Value, field: &str, action: &str) -> Result<Vec<String>, DispatchError> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(value) => string_array_value(value, field, action),
    }
}

fn string_array_value(
    value: &Value,
    field: &str,
    action: &str,
) -> Result<Vec<String>, DispatchError> {
    value
        .as_array()
        .ok_or_else(|| action_failed(action, format!("`{field}` must be an array")))
        .and_then(|values| {
            values
                .iter()
                .map(|value| {
                    value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                        action_failed(action, format!("`{field}` must contain only strings"))
                    })
                })
                .collect()
        })
}

fn required_string_array(
    input: &Value,
    field: &str,
    action: &str,
) -> Result<Vec<String>, DispatchError> {
    input
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| action_failed(action, format!("`{field}` must be an array")))
        .and_then(|values| {
            values
                .iter()
                .map(|value| {
                    value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                        action_failed(action, format!("`{field}` must contain only strings"))
                    })
                })
                .collect()
        })
}

fn required_string<'a>(
    input: &'a Value,
    field: &str,
    action: &str,
) -> Result<&'a str, DispatchError> {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| action_failed(action, format!("`{field}` must be a non-empty string")))
}

fn action_failed(action: &str, message: impl Into<String>) -> DispatchError {
    DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message: message.into(),
    }
}
