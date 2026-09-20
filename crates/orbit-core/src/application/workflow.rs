use serde_json::Value;

use crate::OrbitError;

pub use orbit_types::workflow::{CompletionPolicy, ShipMode, resolved_ship_mode};

pub struct Workflow {
    pub alias: &'static str,
    pub job_id: &'static str,
}

pub const WORKFLOWS: &[Workflow] = &[
    Workflow {
        alias: "auto",
        job_id: "workspace_auto_pipeline",
    },
    Workflow {
        alias: "ship",
        job_id: "task_auto_pipeline",
    },
    Workflow {
        alias: "task-pilot",
        job_id: "task_pilot_pipeline",
    },
];

pub fn find_workflow(name: &str) -> Option<&'static Workflow> {
    WORKFLOWS.iter().find(|w| w.alias == name)
}

/// Canonical alias of the gated ship workflow (`task_auto_pipeline`).
pub const SHIP_WORKFLOW_ALIAS: &str = "ship";
pub const AUTO_WORKFLOW_ALIAS: &str = "auto";

/// Build the `task_auto_pipeline` input document for a ship run.
///
/// An empty `task_ids` slice selects auto mode (the pipeline discovers
/// backlog tasks itself). Explicit ids are validated for duplicates and
/// emptiness so every submission surface rejects the same malformed input.
///
/// [ORB-11187] `completion` is only written when it departs from the `review`
/// default, so an ordinary submission's persisted input is unchanged and the
/// presence of the key is itself the durable record that an operator granted
/// this run completion authority.
///
/// [ORB-11746] `base_sync` is only written for `--mode local`. Seeded shipping
/// jobs default `base_sync: remote` (fetch `origin/<base>`), which is correct
/// for PR delivery but fails `worktree_setup` on a disposable repo with no
/// remotes. Local mode opts into the local base so First Task can complete
/// without origin; PR submissions omit the key and keep the remote default.
pub fn build_ship_input(
    mode: ShipMode,
    base_branch: &str,
    task_ids: &[String],
    completion: CompletionPolicy,
    allowed_crews: &[String],
) -> Result<Value, OrbitError> {
    if base_branch.trim().is_empty() {
        return Err(OrbitError::InvalidInput(
            "ship base branch must not be empty".to_string(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for task_id in task_ids {
        if task_id.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task id in explicit task selection must not be empty".to_string(),
            ));
        }
        if !seen.insert(task_id.as_str()) {
            return Err(OrbitError::InvalidInput(format!(
                "duplicate task id '{task_id}' in explicit task selection"
            )));
        }
    }

    let mut map = serde_json::Map::new();
    map.insert(
        "mode".to_string(),
        Value::String(mode.as_input_value().to_string()),
    );
    map.insert(
        "base_branch".to_string(),
        Value::String(base_branch.to_string()),
    );
    if mode == ShipMode::Local {
        map.insert("base_sync".to_string(), Value::String("local".to_string()));
    }
    if !task_ids.is_empty() {
        map.insert(
            "task_ids".to_string(),
            Value::Array(task_ids.iter().cloned().map(Value::String).collect()),
        );
    }
    if completion.completes() {
        map.insert(
            "completion".to_string(),
            Value::String(completion.as_input_value().to_string()),
        );
    }
    if !allowed_crews.is_empty() {
        map.insert(
            "allowed_crews".to_string(),
            Value::Array(allowed_crews.iter().cloned().map(Value::String).collect()),
        );
    }
    Ok(Value::Object(map))
}
