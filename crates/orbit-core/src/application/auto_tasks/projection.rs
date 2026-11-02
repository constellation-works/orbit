//! Surface-specific list rows over the shared listing state. Profiles retain
//! the published field presence and text policies without repeating them in adapters.

use super::scheduler::open_auto_task_instances_from_tasks;
use super::{AutoTaskListingRow, AutoTaskSkip, ListedAutoTask};
use crate::application::routines::ScheduleDisplayState;
use crate::runtime::audit::run_logs::bounded_redacted_text;
use crate::{OrbitRuntime, Task};
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::{
    AutoTaskCursor, AutoTaskDefinition, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy,
};
use serde_json::{Value, json};

/// The existing list document contract of each caller. Profiles perform only
/// the reads that caller already required; HTTP supplies its request-wide task scan.
#[derive(Clone, Copy)]
pub enum AutoTaskListingProfile<'a> {
    /// Complete definition plus scheduler fire and condition diagnostics.
    Tool,
    /// Ordinary tool document with always-present CLI listing markers.
    Cli,
    /// Operator overview, without task bodies or condition diagnostics.
    BoundedMcp {
        coordination_refusal: Option<&'a str>,
    },
    /// Dashboard polling row, with instances from one request-wide metadata scan.
    Http {
        minted: Result<&'a [&'a Task], &'a OrbitError>,
        now: DateTime<Utc>,
    },
}

impl AutoTaskListingProfile<'_> {
    /// Finish an already-projected ordinary tool document without rereading state.
    /// This lets the CLI retain its audited tool invocation. Other profiles leave
    /// the document unchanged; their row projection is performed below.
    pub fn project_auto_task_tool_document(self, mut document: Value) -> Value {
        if matches!(self, Self::Cli) {
            match &mut document {
                Value::Array(rows) => {
                    for row in rows {
                        *row = self.project_auto_task_tool_document(std::mem::take(row));
                    }
                }
                Value::Object(fields) => {
                    fields
                        .entry("plugin_inactive")
                        .or_insert(Value::Bool(false));
                    fields.entry("skipped_reason").or_insert(Value::Null);
                }
                _ => {}
            }
        }
        document
    }
}

impl OrbitRuntime {
    /// Project one list row using the caller's explicit public contract.
    pub fn project_auto_task_listing_row(
        &self,
        row: &AutoTaskListingRow,
        profile: AutoTaskListingProfile<'_>,
    ) -> Result<Value, OrbitError> {
        let value = match profile {
            AutoTaskListingProfile::Tool | AutoTaskListingProfile::Cli => {
                let mut value = auto_task_definition_document(
                    &row.listed,
                    row.effective_enabled,
                    row.skipped_reason.as_deref(),
                )?;
                value["last_fire"] =
                    last_fire_json(self, &row.listed.definition, row.cursor.as_ref())?;
                if let Some(condition) = auto_task_condition_document(self, &row.listed.definition)
                {
                    value["condition"] = condition;
                }
                value
            }
            AutoTaskListingProfile::BoundedMcp {
                coordination_refusal,
            } => bounded_listing_json(row, coordination_refusal),
            AutoTaskListingProfile::Http { minted, now } => {
                http_listing_json(self, row, minted, now)
            }
        };
        Ok(profile.project_auto_task_tool_document(value))
    }
}

fn bounded_listing_json(row: &AutoTaskListingRow, coordination_refusal: Option<&str>) -> Value {
    let definition = &row.listed.definition;
    let plugin_inactive = row.skip == Some(AutoTaskSkip::PluginInactive);
    let state = if plugin_inactive {
        "plugin_inactive"
    } else {
        row.schedule_state.as_str()
    };
    let next_due = (!plugin_inactive && row.schedule_state == ScheduleDisplayState::Scheduled)
        .then_some(row.next_slot)
        .flatten();
    json!({
        "name": definition.name,
        "description": bounded_listing_text(&definition.description),
        "enabled": definition.enabled,
        "schedule": definition.schedule,
        "state": state,
        "next_due": next_due,
        "target": bounded_listing_text(&definition.template.title),
        "dedupe": definition.dedupe,
        "skip_reason": row.skipped_reason,
        "effective_enabled": row.effective_enabled,
        "toggle_available": !plugin_inactive && coordination_refusal.is_none(),
        "mint_available": !plugin_inactive && coordination_refusal.is_none(),
        "control_reason": coordination_refusal,
        "updated_at": definition.updated_at,
    })
}

fn bounded_listing_text(value: &str) -> String {
    let (value, truncated) = bounded_redacted_text(value, 4096);
    if truncated {
        format!("{value} [display truncated]")
    } else {
        value
    }
}

/// A condition definition's listing: ownership, counts, any stall, and the
/// last fire and skip. A host that cannot read it lists the reason instead.
pub(crate) fn auto_task_condition_document(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
) -> Option<Value> {
    match runtime.auto_task_condition_listing(definition) {
        Ok(listing) => listing.map(|listing| serde_json::to_value(listing).unwrap_or(Value::Null)),
        Err(error) => Some(json!({"unavailable": error.to_string()})),
    }
}

/// Project only the latest scheduler fire or recorded precondition skip.
fn last_fire_json(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
    cursor: Option<&AutoTaskCursor>,
) -> Result<Value, OrbitError> {
    // Delivery schedules never write the time cursor, so their latest fire
    // lives in scheduler-owned automation state and receipts instead. A
    // leftover cursor from an earlier time schedule must not shadow it.
    if matches!(definition.schedule, AutoTaskSchedule::Deliveries { .. }) {
        return delivery_last_fire_json(runtime, definition);
    }
    // A condition mints through its fire records, never the slot cursor.
    if matches!(definition.schedule, AutoTaskSchedule::Condition { .. }) {
        let listing = runtime
            .auto_task_condition_listing(definition)
            .ok()
            .flatten();
        let fire = listing
            .as_ref()
            .and_then(|listing| listing.last_fire.clone());
        let skip = listing.and_then(|listing| listing.last_skip);
        return Ok(match (fire, skip) {
            (Some(fire), Some(skip)) if skip.at > fire.observed_at => json!({
                "kind": "skipped",
                "at": skip.at.to_rfc3339(),
                "skip_reason": skip.reason,
            }),
            (Some(fire), _) => match fire.action_id {
                Some(task) => fired_json(runtime, fire.observed_at.to_rfc3339(), Some(task))?,
                None => json!({
                    "kind": "fired",
                    "at": fire.observed_at.to_rfc3339(),
                    "fire_state": fire.state,
                }),
            },
            (None, Some(skip)) => json!({
                "kind": "skipped",
                "at": skip.at.to_rfc3339(),
                "skip_reason": skip.reason,
            }),
            (None, None) => Value::Null,
        });
    }
    let Some(cursor) = cursor else {
        return Ok(Value::Null);
    };
    if let Some(skip) = &cursor.last_skip {
        return Ok(json!({
            "kind": "skipped",
            "at": skip.at,
            "skip_reason": skip.reason,
        }));
    }
    let Some(at) = &cursor.last_fired_at else {
        return Ok(Value::Null);
    };
    fired_json(runtime, at.clone(), cursor.last_task_id.clone())
}

/// The latest delivery fire from scheduler-owned records: the active
/// batch's latest minted task, including during retry backoff, or the
/// newest accepted receipt's task. Only bounded time/task/status fields are projected;
/// no run ids, logs or step output are read.
fn delivery_last_fire_json(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
) -> Result<Value, OrbitError> {
    // Without a registered machine identity this host owns no delivery
    // consumer, so it cannot have a delivery fire to report.
    let Ok(consumer) =
        crate::application::automation::consumer_key(runtime, "auto-task", &definition.name)
    else {
        return Ok(Value::Null);
    };
    let store = runtime.automation_store()?;
    let state = store.automation_state(&consumer)?;
    let active = if let Some(attempt) = state.as_ref().and_then(|state| state.active.as_ref()) {
        let tasks = runtime.stores().tasks();
        let task_id = if let Some(id) = &attempt.action_id {
            Some(id.clone())
        } else if let Some(task) = tasks.automation_task_for_key(&attempt.action_key)? {
            Some(task.id)
        } else if attempt.attempt > 1
            && (attempt.retry_after.is_some() || attempt.reissue.is_some())
        {
            let key = format!("automation:{}:{}", attempt.batch.id, attempt.attempt - 1);
            tasks
                .automation_task_for_key(&key)?
                .map(|task| task.id)
                .or_else(|| {
                    attempt
                        .reissue
                        .as_ref()
                        .and_then(|reissue| reissue.from_action_id.clone())
                })
        } else {
            None
        };
        task_id.map(|id| (attempt.batch.created_at, id))
    } else {
        None
    };
    // Receipts return newest first, so one row bounds the completed case.
    let receipt = store
        .automation_receipts(&consumer, 1)?
        .into_iter()
        .next()
        .map(|receipt| (receipt.accepted_at, receipt.action_id));
    // Compare the actual fire times when tasks still exist; a receipt may
    // have been accepted after the newer batch was frozen.
    let fire_time =
        |record: &(chrono::DateTime<chrono::Utc>, String)| match runtime.get_task(&record.1) {
            Ok(task) => Ok(task.created_at),
            Err(OrbitError::NotFound { .. }) => Ok(record.0),
            Err(error) => Err(error),
        };
    let latest = match (active, receipt) {
        (Some(active), Some(receipt)) => {
            if fire_time(&active)? >= fire_time(&receipt)? {
                Some(active)
            } else {
                Some(receipt)
            }
        }
        (Some(active), None) => Some(active),
        (None, Some(receipt)) => Some(receipt),
        (None, None) => None,
    };
    let Some((recorded_at, task_id)) = latest else {
        return Ok(Value::Null);
    };
    // Prefer the minted task's own creation time (the actual fire time,
    // including a reissued attempt over an older batch); the recorded
    // batch/receipt time is the fallback when the task is gone.
    match runtime.get_task(&task_id) {
        Ok(task) => Ok(json!({
            "kind": "fired",
            "at": task.created_at,
            "task_id": task_id,
            "task_status": task.status,
        })),
        Err(OrbitError::NotFound { .. }) => Ok(json!({
            "kind": "fired",
            "at": recorded_at,
            "task_id": task_id,
            "task_status": Value::Null,
        })),
        Err(error) => Err(error),
    }
}

fn fired_json(
    runtime: &OrbitRuntime,
    at: String,
    task_id: Option<String>,
) -> Result<Value, OrbitError> {
    let task_status = match task_id.as_deref() {
        Some(task_id) => match runtime.get_task(task_id) {
            Ok(task) => Some(task.status),
            Err(OrbitError::NotFound { .. }) => None,
            Err(error) => return Err(error),
        },
        None => None,
    };
    Ok(json!({
        "kind": "fired",
        "at": at,
        "task_id": task_id,
        "task_status": task_status,
    }))
}

/// The canonical definition record, plus the inactive-plugin marker when its
/// seeding plugin is off here and the reason when it never fires here. A
/// live definition is the bare record.
pub(crate) fn auto_task_definition_document(
    listed: &ListedAutoTask,
    effective_enabled: bool,
    skipped_reason: Option<&str>,
) -> Result<Value, OrbitError> {
    let mut value = serde_json::to_value(&listed.definition)
        .map_err(|error| OrbitError::Io(format!("encode auto-task: {error}")))?;
    value["effective_enabled"] = json!(effective_enabled);
    let Some(object) = value.as_object_mut() else {
        return Ok(value);
    };
    if let Some(inactive) = &listed.inactive_plugin {
        object.insert("plugin_inactive".to_string(), json!(true));
        object.insert("inactive_plugin".to_string(), json!(inactive));
    }
    if let Some(reason) = skipped_reason {
        object.insert("skipped_reason".to_string(), json!(reason));
    }
    Ok(value)
}

fn http_listing_json(
    runtime: &OrbitRuntime,
    row: &AutoTaskListingRow,
    minted: Result<&[&Task], &OrbitError>,
    now: DateTime<Utc>,
) -> Value {
    let listed = &row.listed;
    let definition = &listed.definition;
    let cursor = row.cursor.as_ref();
    let open_duplicate = minted.as_ref().is_ok_and(|tasks| {
        open_auto_task_instances_from_tasks(tasks.iter().copied())
            .next()
            .is_some()
    });
    let automation = match &definition.schedule {
        AutoTaskSchedule::Deliveries { .. } => Some(
            if let Err(error) = minted
                && definition.dedupe == DedupePolicy::SkipIfOpen
            {
                json!({"reason":"state_unavailable","error":error.to_string()})
            } else {
                match crate::application::automation::inspect_auto_task_with_open_instance(
                    runtime,
                    definition,
                    now,
                    open_duplicate,
                ) {
                    Ok(diagnostic) => {
                        crate::application::automation::projection::automation_summary(&json!(
                            diagnostic
                        ))
                    }
                    Err(error) => json!({"reason":"state_unavailable","error":error.to_string()}),
                }
            },
        ),
        _ => None,
    };
    let last_minted = minted.ok().and_then(|tasks| tasks.first());
    let last_minted_task_id = last_minted
        .map(|task| task.id.to_string())
        .or_else(|| cursor.and_then(|cursor| cursor.last_task_id.clone()));
    let last_minted_task_status = last_minted.map(|task| task.status);
    json!({
        "name": definition.name,
        "description": definition.description,
        "enabled": definition.enabled,
        "effective_enabled": row.effective_enabled,
        "schedule": definition.schedule,
        "schedule_summary": schedule_summary(&definition.schedule),
        "automation": automation,
        // A condition definition's ownership, arm counts, stall and last
        // fire and skip.
        "condition": match runtime.auto_task_condition_listing(definition) {
            Ok(listing) => json!(listing),
            Err(error) => json!({"unavailable": error.to_string()}),
        },
        "template_summary": template_summary(&definition.template),
        "template": {
            "title": definition.template.title,
            "crew": definition.template.crew,
            "status": definition.template.status,
            "priority": definition.template.priority,
            "complexity": definition.template.complexity,
            "required_tools": definition.template.required_tools,
        },
        "dedupe": match definition.dedupe {
            DedupePolicy::SkipIfOpen => "skip_if_open",
            DedupePolicy::Always => "always",
        },
        "last_evaluation": cursor.map(|cursor| json!({
            "kind": if cursor.last_fired_at.is_some() { "fired" } else { "baselined" },
            "baseline_at": cursor.baseline_at,
            "last_slot": cursor.last_slot,
            "last_fired_at": cursor.last_fired_at,
            "last_task_id": cursor.last_task_id,
            "pending": cursor.pending.as_ref().map(|pending| json!({
                "slot": pending.slot,
                "task_id": pending.task_id,
            })),
            // Why a due definition minted nothing, so a quiet panel is
            // explained rather than merely empty [ORB-12698].
            "last_skip": cursor.last_skip.as_ref().map(|skip| json!({
                "at": skip.at,
                "slot": skip.slot,
                "reason": skip.reason,
                "ref": skip.reference,
                "cursor_sha": skip.cursor_sha,
                "tip_sha": skip.tip_sha,
                "cursor_task_id": skip.cursor_task_id,
                "detail": skip.detail,
            })),
        })),
        "last_minted_task_id": last_minted_task_id,
        "last_minted_task_status": last_minted_task_status,
        "next_evaluation": next_evaluation_projection(row, automation.as_ref()),
        "open_duplicate": open_duplicate,
        "may_create_open_duplicate": open_duplicate,
        "plugin_inactive": listed.inactive_plugin.is_some(),
        "inactive_plugin": listed.inactive_plugin,
        "skipped_reason": row.skipped_reason,
    })
}

fn schedule_summary(schedule: &AutoTaskSchedule) -> String {
    let base = match schedule {
        AutoTaskSchedule::Deliveries {
            deliveries_landed: t,
            ..
        } => format!(
            "{} deliveries on {} ({})",
            t.threshold, t.branch, t.coverage
        ),
        AutoTaskSchedule::Cron { cron, .. } => format!("cron {cron}"),
        AutoTaskSchedule::Interval { every_minutes, .. } if *every_minutes == 1 => {
            "every 1 minute".to_string()
        }
        AutoTaskSchedule::Interval { every_minutes, .. } => {
            format!("every {every_minutes} minutes")
        }
        AutoTaskSchedule::Condition { condition } => {
            format!("{} condition: {}", condition.scope.as_str(), condition.when)
        }
    };
    match schedule.guard().and_then(|guard| guard.when.as_deref()) {
        Some(when) => format!("{base}, when {when}"),
        None => base,
    }
}

fn template_summary(template: &AutoTaskTemplate) -> String {
    let title = if template.title.starts_with("[auto-task] ") {
        template.title.clone()
    } else {
        format!("[auto-task] {}", template.title)
    };
    let mut parts = vec![title];
    if let Some(crew) = template.crew.as_deref() {
        parts.push(format!("crew {crew}"));
    }
    parts.push(format!("status {}", template.status));
    parts.push(format!("priority {}", template.priority));
    parts.push(format!(
        "complexity {}",
        template
            .complexity
            .map_or("unassessed", |value| value.as_str())
    ));
    parts.join(" · ")
}

/// The shared schedule state, with a delivery trigger whose state the panel's
/// inspection could not read labelled unavailable.
fn next_evaluation_projection(row: &AutoTaskListingRow, automation: Option<&Value>) -> Value {
    let (state, at) = row.schedule_with_trigger(!automation_unavailable(automation));
    state.next_evaluation(at.map(|slot| slot.to_rfc3339()))
}

fn automation_unavailable(automation: Option<&Value>) -> bool {
    automation
        .and_then(|value| value.get("reason"))
        .and_then(Value::as_str)
        .is_some_and(|reason| reason == "source_unavailable" || reason == "state_unavailable")
}
