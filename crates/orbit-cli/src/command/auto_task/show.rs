use clap::Args;
use orbit_core::application::auto_tasks::{
    AutoTaskCursor, cursor_state_path, definition_path, load_cursor_state,
};
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

use super::output::definition_to_json;

#[derive(Args)]
pub struct AutoTaskShowArgs {
    /// Definition name
    pub name: String,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
    /// Preview the baseline and source observations without admitting actions.
    #[arg(long)]
    pub preview: bool,
}

impl Execute for AutoTaskShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let definition = runtime.auto_task_show(&self.name)?.ok_or_else(|| {
            OrbitError::InvalidInput(format!("no such auto-task '{}'", self.name))
        })?;

        let mut doc = definition_to_json(&definition);
        let definition_root = runtime.local_root();
        let source_path = definition_path(&definition_root, &self.name);
        doc["definition_source"] = json!({
            "root": definition_root,
            "path": source_path,
        });
        // The host-local cursor explains why a due definition minted nothing:
        // an unreadable file is reported, never rendered as "never observed".
        match load_cursor_state(&cursor_state_path(&runtime.paths().state_dir)) {
            Ok(state) => {
                doc["cursor"] = state
                    .definitions
                    .get(&self.name)
                    .map(cursor_to_json)
                    .unwrap_or(Value::Null);
            }
            Err(error) => doc["cursor_state_error"] = json!(error.to_string()),
        }

        if matches!(
            definition.schedule,
            orbit_core::AutoTaskSchedule::Deliveries { .. }
        ) {
            doc["automation"] =
                serde_json::to_value(orbit_core::application::automation::inspect_auto_task(
                    runtime,
                    &definition,
                    chrono::Utc::now(),
                )?)
                .map_err(|e| OrbitError::InvalidInput(e.to_string()))?;
        }

        if self.preview
            && matches!(
                definition.schedule,
                orbit_core::AutoTaskSchedule::Deliveries { .. }
            )
        {
            doc["automation"] =
                serde_json::to_value(orbit_core::application::automation::evaluate_auto_task(
                    runtime,
                    &definition,
                    true,
                    chrono::Utc::now(),
                )?)
                .map_err(|e| OrbitError::InvalidInput(e.to_string()))?;
        }
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{} ({})",
            definition.name,
            if definition.enabled {
                "enabled"
            } else {
                "disabled"
            }
        );
        if !definition.description.is_empty() {
            let _ = writeln!(out, "  {}", definition.description);
        }
        let _ = writeln!(out, "  definition root: {}", definition_root.display());
        let _ = writeln!(out, "  definition source: {}", source_path.display());
        let _ = writeln!(
            out,
            "  schedule: {}",
            super::output::schedule_summary(&definition)
        );
        let _ = writeln!(out, "  dedupe: {}", definition.dedupe);
        let _ = writeln!(out, "  template: {}", definition.template.title);
        if let Some(skip) = definition.skip_if_unchanged.as_ref() {
            let _ = writeln!(
                out,
                "  skip_if_unchanged: ref {} cursor tags [{}]",
                skip.reference,
                skip.cursor.tags.join(", ")
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
}

/// The host-local cursor as stored, including `last_skip` — the most recent
/// mint-time precondition skip and the two SHAs it was decided on.
fn cursor_to_json(cursor: &AutoTaskCursor) -> Value {
    serde_json::to_value(cursor).unwrap_or(Value::Null)
}
