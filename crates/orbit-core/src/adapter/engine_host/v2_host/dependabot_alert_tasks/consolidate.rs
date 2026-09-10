//! Preview and apply consolidation of unstarted per-alert Code scanning tasks
//! into the bounded remediation groups the sweep now files.
//!
//! Sweeps that ran before grouping left one task per alert, so a backlog can
//! hold a dozen records for one repair. This action folds those records into
//! one group task per shared cause. It previews by default: an operator sees
//! the exact source task IDs and the replacement before anything is written.
//!
//! Three rules keep it safe to re-run against a live backlog. Only *unstarted*
//! tasks (proposed or backlog) are candidates — work someone has started,
//! deferred, or already grouped is reported and left exactly as it is. Every
//! source is re-read before the group is minted, and the group is abandoned if
//! any of them moved, so a task that started between preview and apply is not
//! rewritten. And the replacement carries each member's per-alert coverage key
//! while the sources are rejected with a covering-task comment — the evidence
//! both the sweep's exact-key path and its confirmed-duplicate path already
//! understand, so a second run finds nothing left to consolidate.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskRelation, TaskRelationType, TaskStatus};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::TaskAddParams;

use super::code_groups::{
    CodeAlertGroup, CodeGroupTaskRequest, cause_key, code_group_task_params, group_alert_numbers,
    group_bounds, group_code_alerts, group_paths,
};
use super::{CODE_KEY_PREFIX, CODE_TAG, SYSTEM_CREW, bounded_u64, field};

const DEFAULT_MAX_GROUPS: u64 = 10;
const MAX_GROUPS: u64 = 50;

/// A per-alert task the consolidation can fold into a group.
struct SourceTask {
    id: String,
    repository: String,
    alert: Value,
    dependencies: Vec<String>,
    context_files: Vec<String>,
}

/// A replacement group and the exact tasks it would replace.
struct Consolidation {
    group: CodeAlertGroup,
    repository: String,
    source_ids: Vec<String>,
    dependencies: Vec<String>,
    context_files: Vec<String>,
}

pub(crate) fn consolidate_code_scanning_tasks(
    runtime: &OrbitRuntime,
    input: &Value,
) -> Result<Value, OrbitError> {
    let apply = input
        .get("apply")
        .and_then(Value::as_bool)
        .unwrap_or_default();
    let max_groups = bounded_u64(input, "max_groups", DEFAULT_MAX_GROUPS, MAX_GROUPS)? as usize;

    let mut tasks = runtime.list_tasks_by_tags(&[CODE_TAG.to_string()])?;
    tasks.sort_by(|left, right| left.id.cmp(&right.id));

    let mut skipped = Vec::new();
    let mut sources = Vec::new();
    for task in &tasks {
        match classify(task) {
            Ok(Some(source)) => sources.push(source),
            Ok(None) => {}
            Err(reason) => skipped.push(json!({
                "task_id": task.id, "status": task.status.to_string(), "reason": reason,
            })),
        }
    }
    let scanned = sources.len();
    let (consolidations, unchanged) = plan(sources);

    let crew = runtime
        .validate_crew_name(Some(SYSTEM_CREW))
        .is_ok()
        .then(|| SYSTEM_CREW.to_string());
    let mut groups = Vec::new();
    for consolidation in consolidations.iter().take(max_groups) {
        let params = replacement_params(runtime, consolidation, crew.clone());
        let mut entry = describe(consolidation, &params);
        if apply {
            apply_one(runtime, consolidation, params, &mut entry)?;
        }
        groups.push(entry);
    }

    let outcome = match (apply, groups.is_empty()) {
        (_, true) => "nothing_to_consolidate",
        (true, false) => "applied",
        (false, false) => "dry_run",
    };

    Ok(json!({
        "outcome": outcome,
        "apply": apply,
        "scanned_source_tasks": scanned,
        "group_count": groups.len(),
        "groups": groups,
        "unchanged_single_source": unchanged,
        "skipped": skipped,
        "max_groups": max_groups,
        "group_bounds": group_bounds(),
    }))
}

/// Decide what a swept Code scanning task is to this action: a candidate, an
/// untouchable record with a stated reason, or closed history to ignore.
///
/// The status arm is exhaustive on purpose. Whether a status may be rewritten
/// is the whole safety question here, so a new one has to be answered rather
/// than defaulted. Proposed and backlog are the unstarted statuses, and both
/// are statuses [`OrbitRuntime::reject_task`] retires under a compare-and-set.
fn classify(task: &Task) -> Result<Option<SourceTask>, &'static str> {
    match task.status {
        TaskStatus::Proposed | TaskStatus::Backlog => {}
        TaskStatus::Done | TaskStatus::Archived | TaskStatus::Rejected => return Ok(None),
        TaskStatus::Someday => return Err("deferred"),
        TaskStatus::InProgress | TaskStatus::Review | TaskStatus::Blocked => {
            return Err("active_work");
        }
    }
    if alert_key_count(task) != 1 {
        return Err("already_grouped");
    }
    let Some((repository, alert)) = parse_alert_evidence(&task.description) else {
        return Err("unparsable_evidence");
    };
    Ok(Some(SourceTask {
        id: task.id.clone(),
        repository,
        alert,
        dependencies: task.dependencies(),
        context_files: task.context_files.clone(),
    }))
}

fn alert_key_count(task: &Task) -> usize {
    task.tags
        .iter()
        .filter(|tag| tag.starts_with(CODE_KEY_PREFIX))
        .count()
}

/// Group the candidates by shared cause. A group replaces work only when it
/// folds at least two distinct source tasks; a lone task is already the
/// smallest record for its cause and is reported unchanged.
fn plan(sources: Vec<SourceTask>) -> (Vec<Consolidation>, Vec<Value>) {
    let mut by_repository: BTreeMap<String, Vec<SourceTask>> = BTreeMap::new();
    for source in sources {
        by_repository
            .entry(source.repository.clone())
            .or_default()
            .push(source);
    }

    let mut consolidations = Vec::new();
    let mut unchanged = Vec::new();
    for (repository, sources) in by_repository {
        let mut owners: BTreeMap<String, SourceTask> = BTreeMap::new();
        let mut alerts = Vec::new();
        for source in sources {
            alerts.push(source.alert.clone());
            owners.insert(cause_alert_id(&repository, &source.alert), source);
        }

        for group in group_code_alerts(&repository, alerts) {
            let members = group
                .alerts
                .iter()
                .filter_map(|alert| owners.get(&cause_alert_id(&repository, alert)))
                .collect::<Vec<_>>();
            let source_ids = members
                .iter()
                .map(|source| source.id.clone())
                .collect::<BTreeSet<_>>();
            if source_ids.len() < 2 {
                unchanged.extend(source_ids.into_iter().map(
                    |task_id| json!({"task_id": task_id, "reason": "sole_task_for_its_cause"}),
                ));
                continue;
            }
            consolidations.push(Consolidation {
                repository: repository.clone(),
                source_ids: source_ids.into_iter().collect(),
                dependencies: union_of(members.iter().map(|source| &source.dependencies)),
                context_files: union_of(members.iter().map(|source| &source.context_files)),
                group,
            });
        }
    }
    (consolidations, unchanged)
}

/// Cause plus alert number: the identity that maps a grouped alert back to the
/// task it came from, without assuming alert numbers are unique across causes.
fn cause_alert_id(repository: &str, alert: &Value) -> String {
    format!(
        "{}#{}",
        cause_key(repository, alert),
        field(alert, "number")
    )
}

fn union_of<'a>(values: impl Iterator<Item = &'a Vec<String>>) -> Vec<String> {
    values
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn describe(consolidation: &Consolidation, params: &TaskAddParams) -> Value {
    let lead = &consolidation.group.alerts[0];
    json!({
        "cause_key": consolidation.group.cause_key,
        "repository": consolidation.repository,
        "rule_id": field(lead, "rule_id"),
        "message": field(lead, "message"),
        "alert_numbers": group_alert_numbers(&consolidation.group),
        "paths": group_paths(&consolidation.group),
        "source_task_ids": consolidation.source_ids,
        "carried_dependencies": consolidation.dependencies,
        "replacement_title": params.title,
        "replacement_priority": params.priority.to_string(),
    })
}

/// Mint the replacement, then retire each source with a covering-task comment.
///
/// Every source is re-read and re-classified first, and a group whose sources
/// no longer qualify is abandoned whole rather than partially applied, so
/// running work is never rewritten and no alert ends up owned twice. A source
/// that slips past that check and refuses the rejection is reported rather
/// than failing the run:
/// the replacement is already correct, and the next pass sees the leftover as
/// the sole task for its cause and leaves it alone.
fn apply_one(
    runtime: &OrbitRuntime,
    consolidation: &Consolidation,
    params: TaskAddParams,
    entry: &mut Value,
) -> Result<(), OrbitError> {
    for source_id in &consolidation.source_ids {
        let source = runtime.get_task(source_id)?;
        if !matches!(classify(&source), Ok(Some(_))) {
            entry["applied"] = json!(false);
            entry["not_applied_reason"] = json!("source_task_started");
            entry["moved_source_task_id"] = json!(source_id);
            return Ok(());
        }
    }

    let replacement = runtime.add_task(params)?;
    let mut not_retired = Vec::new();
    for source_id in &consolidation.source_ids {
        let outcome = runtime.reject_task(
            source_id,
            format!("Consolidated into covering task {}", replacement.id),
            Some(format!(
                "Consolidated into covering task {}: the same rule, scanner message, and analysis cover every alert in that group, so one task now owns the shared repair. This task's per-alert evidence and dependencies were carried over.",
                replacement.id
            )),
        );
        if let Err(error) = outcome {
            not_retired.push(json!({"task_id": source_id, "error": error.to_string()}));
        }
    }

    entry["applied"] = json!(true);
    entry["replacement_task_id"] = json!(replacement.id);
    entry["sources_not_retired"] = json!(not_retired);
    Ok(())
}

fn replacement_params(
    runtime: &OrbitRuntime,
    consolidation: &Consolidation,
    crew: Option<String>,
) -> TaskAddParams {
    let snapshot = json!({"repository": {"full_name": consolidation.repository}});
    let mut params = code_group_task_params(&CodeGroupTaskRequest {
        snapshot: &snapshot,
        group: &consolidation.group,
        covered_siblings: &[],
        workspace_root: &runtime.paths().repo_root,
        crew,
    });

    // The sources' own selectors are kept alongside the ones the alert paths
    // resolve to: a task-pilot pass may have widened a source's scope to the
    // file that actually carries the repair, and that judgment outlives the
    // record it was made on.
    params.context_files =
        union_of([&params.context_files, &consolidation.context_files].into_iter());
    params.dependencies = consolidation.dependencies.clone();
    params.relations = consolidation
        .source_ids
        .iter()
        .map(|target| TaskRelation {
            relation_type: TaskRelationType::Supersedes,
            target: target.clone(),
        })
        .collect();
    params
}

/// Read a swept per-alert task's `## Alert evidence` block back into the alert
/// shape the group builder consumes.
///
/// Only that section is read, so a later section cannot inject a key, and
/// every field the cause signature depends on is required — a record edited
/// past recognition is reported as unparsable instead of being grouped on
/// partial evidence.
fn parse_alert_evidence(description: &str) -> Option<(String, Value)> {
    let fields = evidence_fields(description);
    let text = |key: &str| fields.get(key).cloned();
    let quoted = |key: &str| fields.get(key).and_then(|value| backticks(value));

    let repository = quoted("Repository")?;
    let number = quoted("Alert")?
        .trim_start_matches('#')
        .parse::<u64>()
        .ok()?;
    let (rule_id, rule_name) = split_name_and_parenthetical(&text("Rule")?)?;
    let [tool_name, tool_version, tool_guid] =
        <[String; 3]>::try_from(backtick_segments(&text("Tool")?)).ok()?;
    let (path, lines) = parse_location(&text("Location")?)?;

    Some((
        repository,
        json!({
            "number": number,
            "rule_id": rule_id,
            "rule_name": rule_name,
            "security_severity": quoted("Security severity")?,
            "tool_name": tool_name,
            "tool_version": tool_version,
            "tool_guid": tool_guid,
            "message": text("Message")?,
            "ref": quoted("Ref")?,
            "commit_sha": quoted("Commit")?,
            "path": path,
            "start_line": lines.map(|(start, _)| start),
            "end_line": lines.map(|(_, end)| end),
            "created_at": quoted("Created").unwrap_or_default(),
            "updated_at": quoted("Updated").unwrap_or_default(),
            "html_url": text("Alert URL").unwrap_or_default(),
        }),
    ))
}

fn evidence_fields(description: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    let mut inside = false;
    for line in description.lines() {
        let line = line.trim();
        if let Some(heading) = line.strip_prefix("## ") {
            if inside {
                break;
            }
            inside = heading == "Alert evidence";
            continue;
        }
        if !inside {
            continue;
        }
        if let Some(entry) = line.strip_prefix("- ")
            && let Some((key, value)) = entry.split_once(": ")
        {
            fields
                .entry(key.trim().to_string())
                .or_insert_with(|| value.trim().to_string());
        }
    }
    fields
}

fn backticks(value: &str) -> Option<String> {
    backtick_segments(value).into_iter().next()
}

fn backtick_segments(value: &str) -> Vec<String> {
    value
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// `` `rust/sql-injection` (SQL injection) `` — the rendered rule ID and name.
fn split_name_and_parenthetical(value: &str) -> Option<(String, String)> {
    let name = backticks(value)?;
    let parenthetical = value
        .split_once('(')
        .and_then(|(_, rest)| rest.rsplit_once(')'))
        .map(|(inside, _)| inside.trim().to_string())
        .unwrap_or_default();
    Some((name, parenthetical))
}

/// `` `src/db.rs` lines 17-19 ``, `` `src/db.rs` line 426 ``, or a backticked
/// path with no line suffix — an alert GitHub reported without a location
/// range. Absent lines stay absent rather than becoming line zero.
fn parse_location(value: &str) -> Option<(String, Option<(u64, u64)>)> {
    let path = backticks(value)?;
    let suffix = value.rsplit('`').next().unwrap_or_default().trim();
    let Some(range) = suffix
        .strip_prefix("lines ")
        .or_else(|| suffix.strip_prefix("line "))
    else {
        return Some((path, None));
    };
    let (start, end) = range.split_once('-').unwrap_or((range, range));
    Some((
        path,
        Some((start.trim().parse().ok()?, end.trim().parse().ok()?)),
    ))
}
