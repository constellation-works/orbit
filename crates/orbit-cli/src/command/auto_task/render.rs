//! Text rendering for auto-task responses.

use std::fmt::Write as _;

use orbit_common::governance::auto_task::AutoTaskVerb;
use orbit_core::OrbitError;
use orbit_types::workflow::automation::recovery::{AutomationStall, RecoveryPreview, ResetPreview};
use serde_json::Value;

use super::prepare::StateFilter;
use crate::command::{CommandOut, Payload};
use crate::output::table::{Column, Table};

/// Build an auto-task response's payload. `name` is the requested definition.
pub(super) fn render(
    verb: AutoTaskVerb,
    name: &str,
    value: Value,
    filter: StateFilter,
) -> CommandOut {
    match verb {
        AutoTaskVerb::Add | AutoTaskVerb::Update => {
            let mut text = text(&value, "name");
            for warning in strings(&value["warnings"]) {
                let _ = write!(text, "\nwarning: {warning}");
            }
            Ok(Payload::detail(value, text).into())
        }
        AutoTaskVerb::List => list(value, filter),
        AutoTaskVerb::Show => show(value),
        AutoTaskVerb::Toggle => {
            let state = if value["enabled"].as_bool() == Some(true) {
                "enabled"
            } else {
                "disabled"
            };
            let text = format!("{} {state}", text(&value, "name"));
            Ok(Payload::detail(value, text).into())
        }
        AutoTaskVerb::Delete => {
            let text = delete_summary(&value);
            Ok(Payload::detail(value, text).into())
        }
        AutoTaskVerb::Restore => {
            let text = format!("{} restored from the shipped default", text(&value, "name"));
            Ok(Payload::detail(value, text).into())
        }
        AutoTaskVerb::Mint => {
            let text = format!("{} {}", text(&value, "id"), text(&value, "title"));
            Ok(Payload::detail(value, text).into())
        }
        AutoTaskVerb::Recover => {
            let preview: RecoveryPreview = decode(&value)?;
            let text = recover_summary(name, &preview);
            Ok(Payload::detail(value, text).into())
        }
        AutoTaskVerb::Reset => {
            let preview: ResetPreview = decode(&value)?;
            let text = reset_summary(name, &preview);
            Ok(Payload::detail(value, text).into())
        }
    }
}

fn list(value: Value, filter: StateFilter) -> CommandOut {
    let Value::Array(rows) = value else {
        return Ok(Payload::document(value).into());
    };
    let records: Vec<Value> = rows
        .into_iter()
        .filter(|row| {
            let enabled = row["effective_enabled"].as_bool() == Some(true);
            (!filter.enabled_only || enabled) && (!filter.disabled_only || !enabled)
        })
        .map(with_listing_markers)
        .collect();

    let mut table = Table::new(vec![
        Column::new("NAME").fixed(),
        Column::new("STATE").fixed().filtered(true),
        Column::new("SCHEDULE").fixed(),
        Column::new("TITLE"),
    ])
    .empty_message("no auto-task definitions");
    for record in &records {
        table.add_row(vec![
            text(record, "name"),
            state_label(record).to_string(),
            schedule_summary(&record["schedule"]),
            text(&record["template"], "title"),
        ]);
    }
    for record in &records {
        if let Some(reason) = record["skipped_reason"].as_str() {
            let label = if is_plugin_inactive(record) {
                "inactive"
            } else {
                "skipped"
            };
            table = table.trailing_notice(format!("{label} [{}]: {reason}", text(record, "name")));
        }
    }
    Ok(Payload::list(records, table).into())
}

/// The tool marks only an inactive or skipped definition; the CLI record
/// always carries the markers.
fn with_listing_markers(mut record: Value) -> Value {
    if let Some(object) = record.as_object_mut() {
        object
            .entry("plugin_inactive")
            .or_insert(Value::Bool(false));
        object.entry("skipped_reason").or_insert(Value::Null);
    }
    record
}

fn state_label(record: &Value) -> &'static str {
    if is_plugin_inactive(record) {
        "inactive"
    } else if record["skipped_reason"].is_string() {
        "skipped"
    } else if record["effective_enabled"].as_bool() == Some(true) {
        "enabled"
    } else {
        "disabled"
    }
}

fn is_plugin_inactive(record: &Value) -> bool {
    record["plugin_inactive"].as_bool() == Some(true)
}

fn show(value: Value) -> CommandOut {
    let mut doc = with_listing_markers(value);
    if let Some(object) = doc.as_object_mut() {
        object.entry("inactive_plugin").or_insert(Value::Null);
    }
    let mut out = String::new();
    let state = if is_plugin_inactive(&doc) {
        "inactive"
    } else if doc["enabled"].as_bool() == Some(true) {
        "enabled"
    } else if doc["effective_enabled"].as_bool() == Some(true) {
        "enabled by the deprecated operation.review_policy = after-landing"
    } else {
        "disabled"
    };
    let _ = writeln!(out, "{} ({state})", text(&doc, "name"));
    if let Some(reason) = doc["skipped_reason"].as_str() {
        let _ = writeln!(out, "  inactive: {reason}");
    }
    if let Some(description) = doc["description"].as_str().filter(|text| !text.is_empty()) {
        let _ = writeln!(out, "  {description}");
    }
    let source = &doc["definition_source"];
    let _ = writeln!(out, "  definition root: {}", text(source, "root"));
    let _ = writeln!(out, "  definition source: {}", text(source, "path"));
    if let Some(layering) = doc.get("layering") {
        let _ = writeln!(out, "  body: {}", body_summary(layering));
        if let Some(settings) = layering.get("settings") {
            let fields = settings_fields(settings);
            let _ = writeln!(
                out,
                "  settings: {}",
                if fields.is_empty() {
                    "no overrides".to_string()
                } else {
                    fields.join(", ")
                }
            );
        }
    } else {
        let _ = writeln!(out, "  body: unknown ({})", text(&doc, "layering_error"));
    }
    let _ = writeln!(out, "  schedule: {}", schedule_summary(&doc["schedule"]));
    let _ = writeln!(out, "  dedupe: {}", text(&doc, "dedupe"));
    let _ = writeln!(out, "  template: {}", text(&doc["template"], "title"));
    if let Some(note) = doc["crew_overridden"].as_str() {
        let _ = writeln!(out, "  crew: {note}");
    }
    if let Some(skip) = doc.get("skip_if_unchanged").filter(|skip| !skip.is_null()) {
        let _ = writeln!(
            out,
            "  skip_if_unchanged: ref {} cursor tags [{}]",
            text(skip, "ref"),
            strings(&skip["cursor"]["tags"]).join(", ")
        );
    }
    if doc["skip_unless_blocked_candidates"].as_bool() == Some(true) {
        let _ = writeln!(
            out,
            "  skip_unless_blocked_candidates: mint only when a block episode awaits triage"
        );
    }
    if let Some(skip) = doc
        .get("cursor")
        .and_then(|cursor| cursor.get("last_skip"))
        .and_then(Value::as_object)
    {
        let text = |key: &str| {
            skip.get(key)
                .and_then(Value::as_str)
                .unwrap_or("-")
                .to_string()
        };
        if let Some(detail) = skip.get("detail").and_then(Value::as_str) {
            let _ = writeln!(
                out,
                "  last skip: {} at {} — {detail}",
                text("reason"),
                text("at"),
            );
        } else {
            let _ = writeln!(
                out,
                "  last skip: {} at {} — {} tip {} covered by cursor {}{}",
                text("reason"),
                text("at"),
                text("ref"),
                text("tip_sha"),
                text("cursor_sha"),
                skip.get("cursor_task_id")
                    .and_then(Value::as_str)
                    .map(|id| format!(" from {id}"))
                    .unwrap_or_default(),
            );
        }
    }
    if let Some(stall) = doc
        .pointer("/automation/state/stall")
        .filter(|stall| !stall.is_null())
    {
        let stall: AutomationStall = decode(stall)?;
        let _ = writeln!(
            out,
            "  stalled: {} since {}{}",
            stall.reason,
            stall.since.to_rfc3339(),
            stall
                .crew_hold
                .map(|hold| format!(" — {}", hold.summary()))
                .unwrap_or_default()
        );
    }
    if let Some(automation) = doc.get("automation") {
        let _ = writeln!(
            out,
            "  automation: {}",
            serde_json::to_string_pretty(automation)
                .map_err(|e| OrbitError::InvalidInput(e.to_string()))?
        );
    }
    Ok(Payload::detail(doc, out).into())
}

fn body_summary(layering: &Value) -> String {
    let forked_fields = strings(&layering["forked_fields"]);
    match layering["body"].as_str() {
        Some("managed") => "managed bundled default".to_string(),
        Some("user_authored") => "user-authored".to_string(),
        Some("forked") if forked_fields.is_empty() => format!(
            "forked from the bundled default in settings only ({}); `orbit workspace sync` moves them to the settings table",
            strings(&layering["settings_fields"]).join(", ")
        ),
        Some("forked") => format!(
            "forked from the bundled default (body fields: {}); upstream template changes no longer apply",
            forked_fields.join(", ")
        ),
        other => other.unwrap_or("unknown").to_string(),
    }
}

/// The fields a settings-table entry overrides, in definition order.
fn settings_fields(settings: &Value) -> Vec<&'static str> {
    [
        ("enabled", "enabled"),
        ("schedule", "schedule"),
        ("dedupe", "dedupe"),
        ("crew", "template.crew"),
        ("priority", "template.priority"),
        ("complexity", "template.complexity"),
        ("tags", "template.tags"),
    ]
    .into_iter()
    .filter(|(key, _)| settings.get(key).is_some_and(|value| !value.is_null()))
    .map(|(_, label)| label)
    .collect()
}

/// A one-line schedule summary.
fn schedule_summary(schedule: &Value) -> String {
    if let Some(trigger) = schedule.get("deliveries_landed") {
        format!(
            "deliveries={} branch={} coverage={}",
            trigger["threshold"],
            text(trigger, "branch"),
            text(trigger, "coverage")
        )
    } else if let Some(cron) = schedule.get("cron") {
        format!("cron={}", cron.as_str().unwrap_or_default())
    } else {
        format!("every={}m", schedule["every_minutes"])
    }
}

fn delete_summary(report: &Value) -> String {
    let mut out = format!("{} deleted", text(report, "name"));
    if report["opted_out"].as_bool() == Some(true) {
        out.push_str(
            "; shipped default opted out of reseeding (`orbit auto-task restore` reinstates it)",
        );
    }
    let consumer = &report["consumer"];
    let released = strings(&consumer["released_refs"]);
    if consumer.is_object() && (consumer["reset"].as_bool() == Some(true) || !released.is_empty()) {
        out.push_str(&format!(
            "; delivery consumer reset, {} pinned refs released",
            released.len()
        ));
    }
    let open_tasks = strings(&report["open_tasks"]);
    if !open_tasks.is_empty() {
        out.push_str(&format!("; left open: {}", open_tasks.join(", ")));
    }
    out
}

fn recover_summary(name: &str, preview: &RecoveryPreview) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{name} ({})", preview.reason);
    let _ = writeln!(out, "  consumer: {}", preview.consumer);
    let _ = writeln!(
        out,
        "  identity: {} -> {}",
        preview.identity.recorded_epoch, preview.identity.configured_epoch
    );
    if !preview.identity.changes.is_empty() {
        let _ = writeln!(out, "  changed: {}", preview.identity.changes.join(", "));
    }

    let debt = &preview.debt;
    let _ = writeln!(out, "  covered: {}", debt.covered.commit);
    if let Some(replay) = &preview.history_replay {
        let _ = writeln!(
            out,
            "  history: {} -> {} ({} mappings, {} added obligations, generation {})",
            replay.old_observed.commit,
            replay.new_observed.commit,
            replay.mappings.len(),
            replay.added_obligations.len(),
            replay.captured_generation
        );
    }
    let _ = writeln!(
        out,
        "  debt: {} pending deliveries, {} pending commits, {} unresolved, {} waived, {} excluded, {} receipts",
        debt.pending_deliveries,
        debt.pending_commits,
        debt.unresolved,
        debt.waived,
        debt.excluded,
        debt.receipts
    );

    if let Some(action) = &preview.action {
        let _ = writeln!(
            out,
            "  action: {} attempt {} ({:?}), {} obligations, {} commits, reissuable={}",
            action.batch_id,
            action.attempt,
            action.state,
            action.obligations.len(),
            action.commits,
            action.reissuable
        );
    }

    if preview.applied.is_empty() {
        let _ = writeln!(out, "  applied: none (preview)");
    } else {
        let _ = writeln!(out, "  applied: {}", preview.applied.join(", "));
    }

    if !preview.refusals.is_empty() {
        let _ = writeln!(out, "  refusals: {}", preview.refusals.join(", "));
    }

    out
}

fn reset_summary(name: &str, preview: &ResetPreview) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{name} ({})", preview.reason);
    let _ = writeln!(out, "  consumer: {}", preview.consumer);
    let _ = writeln!(
        out,
        "  generation: {} (epoch {})",
        preview.generation, preview.epoch
    );

    let debt = &preview.debt;
    let _ = writeln!(
        out,
        "  forgets: {} pending deliveries, {} pending commits, {} unresolved, {} waived, {} excluded, {} receipts",
        debt.pending_deliveries,
        debt.pending_commits,
        debt.unresolved,
        debt.waived,
        debt.excluded,
        debt.receipts
    );
    let _ = writeln!(
        out,
        "  covered: {} observed: {}",
        debt.covered.commit, debt.observed.commit
    );

    if let Some(action) = &preview.action {
        let _ = writeln!(
            out,
            "  action: {} attempt {} ({:?}), {} obligations, {} commits",
            action.batch_id,
            action.attempt,
            action.state,
            action.obligations.len(),
            action.commits
        );
    }

    if let Some(stall) = &preview.stall {
        let _ = writeln!(
            out,
            "  stalled: {} since {}",
            stall.reason,
            stall.since.to_rfc3339()
        );
    }

    let _ = writeln!(out, "  re-baselines at: {}", preview.baseline.commit);

    if preview.applied {
        let _ = writeln!(out, "  applied: reset");
    } else {
        let _ = writeln!(out, "  applied: none (preview; pass --reason to apply)");
    }

    if !preview.refusals.is_empty() {
        let _ = writeln!(out, "  refusals: {}", preview.refusals.join(", "));
    }

    out
}

fn decode<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, OrbitError> {
    serde_json::from_value(value.clone())
        .map_err(|error| OrbitError::InvalidInput(format!("decode auto-task response: {error}")))
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}
