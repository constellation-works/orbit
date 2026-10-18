//! The implementer's `regression_from` output field.
//!
//! A claimed implementer cannot write its own task's relations: the owner
//! holds the task, and `orbit.task.update` is denied in claimed mode. When
//! its task asks it to attribute a regression, it returns the culprit task
//! IDs here instead. The claim handoff carries them to the owner, which
//! checks each against its own workspace and records the `regression_from`
//! relations when it accepts the handoff. The implement step boundary and
//! the handoff both read the field through [`implementer_regression_from`],
//! so a malformed value fails before any commit, push or PR.

use serde_json::Value;

use crate::task::is_valid_orb_task_id;

/// The implementer output field naming the claimed task's culprit tasks.
pub const REGRESSION_FROM_FIELD: &str = "regression_from";

/// The culprit task IDs the implementer returned, trimmed and deduplicated in
/// order. Absent or `null` is none. A single string is accepted as one ID.
/// Anything else, or an entry that is not a task ID, is refused with the
/// reason.
pub fn implementer_regression_from(output: &Value) -> Result<Vec<String>, String> {
    let entries = match output.get(REGRESSION_FROM_FIELD) {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::String(id)) => vec![Value::String(id.clone())],
        Some(Value::Array(entries)) => entries.clone(),
        Some(_) => {
            return Err(format!(
                "the implementer's `{REGRESSION_FROM_FIELD}` must be an array of culprit task IDs"
            ));
        }
    };
    let mut targets = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let target = entry.as_str().map(str::trim).unwrap_or_default();
        if !is_valid_orb_task_id(target) {
            return Err(format!(
                "`{REGRESSION_FROM_FIELD}[{index}]` must be a task ID such as `ORB-<number>`"
            ));
        }
        if !targets.iter().any(|known| known == target) {
            targets.push(target.to_string());
        }
    }
    Ok(targets)
}
