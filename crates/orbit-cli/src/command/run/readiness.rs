//! `orbit run readiness` read-only auto-drain diagnostic.

use clap::Args;
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::Value;

use crate::command::{Block, CommandOut, Execute, Payload};

const DEFAULT_LIMIT: usize = 50;

#[derive(Args)]
#[command(
    about = "Explain why backlog tasks can or cannot start in auto-drain",
    override_usage = "orbit run readiness [<TASK_ID>...] [OPTIONS]",
    after_help = "Examples:\n  orbit run readiness\n  orbit run readiness TASK-123 TASK-124\n  orbit run readiness --concurrency 8 --json\n  orbit run readiness --allow-crew opus,sonnet\n\nThis is a read-only snapshot. It does not reserve work, reconcile stale runs,\nsubmit a run, or mutate tasks; an eligible task is not guaranteed to start.\n\n`--allow-crew` previews the same restriction `orbit run auto --allow-crew` would\napply: excluded tasks report `crew_not_allowed` with the crew they would run as,\nand the rest keep filling the free slots."
)]
pub struct ReadinessCommand {
    /// Optional task IDs to explain. Omit to inspect a bounded backlog snapshot.
    #[arg(value_name = "TASK_ID", num_args = 0..)]
    pub task_ids: Vec<String>,
    /// Leaf-run concurrency to evaluate. Defaults to auto-drain's default (5).
    #[arg(long, value_name = "N")]
    pub concurrency: Option<u32>,
    /// Maximum tasks to explain, including an explicit selection (1-500).
    #[arg(long, default_value_t = DEFAULT_LIMIT, value_name = "N")]
    pub limit: usize,
    /// Evaluate as if the drain were restricted to these configured crews.
    /// Repeatable and comma-separated; omitted, no crew restriction applies.
    #[arg(long = "allow-crew", value_name = "CREW", value_delimiter = ',')]
    pub allow_crew: Vec<String>,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

impl Execute for ReadinessCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        validate_task_ids(&self.task_ids)?;
        let payload = runtime.workspace_auto_readiness(
            &self.task_ids,
            self.concurrency,
            self.limit,
            &self.allow_crew,
        )?;
        readiness_payload(payload)
    }
}

fn validate_task_ids(task_ids: &[String]) -> Result<(), OrbitError> {
    let mut seen = std::collections::BTreeSet::new();
    for task_id in task_ids {
        if task_id.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task id in readiness selection must not be empty".to_string(),
            ));
        }
        if !seen.insert(task_id) {
            return Err(OrbitError::InvalidInput(format!(
                "duplicate task id '{task_id}' in readiness selection"
            )));
        }
    }
    Ok(())
}

pub(crate) fn readiness_payload(payload: Value) -> CommandOut {
    let lines = readiness_lines(&payload);
    Ok(Payload::blocks(payload, vec![Block::text(lines.join("\n"))]).into())
}

fn readiness_lines(payload: &Value) -> Vec<String> {
    let capacity = &payload["capacity"];
    let mut lines = vec![format!(
        "Snapshot only — eligible does not guarantee a task will start. Active leaf runs: {}/{}; free slots: {}.",
        capacity["active_leaf_runs"], capacity["max_active_leaf_runs"], capacity["free_slots"],
    )];
    if let Some(tasks) = payload["tasks"].as_array() {
        for task in tasks {
            let task_id = task["task_id"].as_str().unwrap_or("-");
            let reason = task["reason"].as_str().unwrap_or("unknown");
            let eligible = task["eligible"].as_bool().unwrap_or(false);
            let crew = task["crew"]
                .as_str()
                .map(|crew| format!(" crew={crew}"))
                .unwrap_or_default();
            lines.push(format!(
                "{task_id}: {} ({reason}){crew}",
                if eligible { "eligible" } else { "waiting" }
            ));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn readiness_selection_rejects_blank_and_duplicate_ids() {
        assert!(validate_task_ids(&[" ".to_string()]).is_err());
        assert!(validate_task_ids(&["ORB-1".to_string(), "ORB-1".to_string()]).is_err());
    }

    #[test]
    fn readiness_payload_names_snapshot_limit_and_reason() {
        let text = readiness_lines(&json!({
            "capacity": { "active_leaf_runs": 5, "max_active_leaf_runs": 5, "free_slots": 0 },
            "tasks": [{ "task_id": "ORB-1", "eligible": false, "reason": "capacity_saturated" }]
        }))
        .join("\n");
        assert!(text.contains("Snapshot only"), "{text}");
        assert!(
            text.contains("ORB-1: waiting (capacity_saturated)"),
            "{text}"
        );
    }
}
