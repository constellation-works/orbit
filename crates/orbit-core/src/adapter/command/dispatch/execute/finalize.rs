//! Success finalization for a dispatched tool call: folds the audit-write
//! outcome into the value the caller receives.

use orbit_common::OrbitError;
use orbit_tools::ToolExecutionKind;
use serde_json::{Value, json};

use super::audit_store::ToolDispatchOutcome;

/// Fold a successful tool call together with its audit-write outcome into the
/// dispatch result.
///
/// On a persisted audit row it returns the value. A read-only call also
/// returns its value when the audit write fails, because it committed
/// nothing. A mutating call whose audit write still fails after the lock
/// retry returns that same success with [`attach_unpersisted_audit_warning`]:
/// the mutation is already committed, so an error would only make a retrying
/// caller duplicate it. The caller is responsible for the per-thread dedup
/// signal, which is set as soon as the row persists regardless of the tool's
/// own outcome.
pub(super) fn finalize_successful_dispatch(
    tool_name: &str,
    execution_kind: ToolExecutionKind,
    value: Value,
    audit_write: Result<(), OrbitError>,
) -> Result<ToolDispatchOutcome, OrbitError> {
    match audit_write {
        Ok(()) => Ok(ToolDispatchOutcome {
            value,
            audit_recorded: true,
        }),
        Err(err) if execution_kind == ToolExecutionKind::ReadOnly => {
            tracing::warn!(
                tool = tool_name,
                "read-only tool completed but its audit event could not be persisted: {err}"
            );
            Ok(ToolDispatchOutcome {
                value,
                audit_recorded: false,
            })
        }
        Err(err) => {
            tracing::error!(
                tool = tool_name,
                "mutating tool committed but its audit row could not be persisted: {err}"
            );
            Ok(ToolDispatchOutcome {
                value: attach_unpersisted_audit_warning(tool_name, value, &err),
                audit_recorded: false,
            })
        }
    }
}

/// Name the committed object on a success value whose audit row is missing.
///
/// The field is `audit_warning`, beside the tool's own document, so a caller
/// that treats a transport error as "try the mutation again" does not retry.
/// An object result gains the field. Any other JSON shape is wrapped, because
/// this path exists only when the alternative is reporting a committed
/// mutation as a failure.
fn attach_unpersisted_audit_warning(tool_name: &str, value: Value, err: &OrbitError) -> Value {
    let id = committed_object_id(&value).map(str::to_string);
    let message = match &id {
        Some(id) => format!(
            "tool '{tool_name}' committed '{id}' but its audit row could not be persisted: {err}"
        ),
        None => {
            format!("tool '{tool_name}' committed but its audit row could not be persisted: {err}")
        }
    };
    let warning = json!({
        "code": "audit_row_unpersisted",
        "message": message,
        "committed": {
            "tool": tool_name,
            "id": id,
        }
    });
    match value {
        Value::Object(mut object) => {
            object.insert("audit_warning".to_string(), warning);
            Value::Object(object)
        }
        other => json!({
            "value": other,
            "audit_warning": warning,
        }),
    }
}

/// Identity a mutating tool's JSON result uses for the object it just wrote.
fn committed_object_id(value: &Value) -> Option<&str> {
    let object = value.as_object()?;
    for key in ["id", "task_id", "friction_id"] {
        if let Some(id) = non_empty_str(object.get(key)) {
            return Some(id);
        }
    }
    non_empty_str(object.get("task").and_then(|task| task.get("id")))
}

fn non_empty_str(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).filter(|id| !id.is_empty())
}
