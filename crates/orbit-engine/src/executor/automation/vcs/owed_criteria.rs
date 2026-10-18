//! An implementer's acceptance criteria owed after deploy.
//!
//! Only a criterion the task pilot typed post-deploy may be handed off owed:
//! no candidate can evidence it before merge, so the delivery proceeds and
//! settles in `review` until an operator records the measurement. The host
//! refuses every other owed claim, so an unmet ordinary criterion still fails
//! the step as it always did.

use orbit_types::workflow::{CONTRACT_REFUSAL_MARKER, OWED_CRITERIA_FIELD, OwedCriterion};
use serde_json::Value;

use crate::context::RuntimeHost;

/// Admit a successful implementer step's owed criteria. `Ok(Some)` is the
/// admitted list, which replaces the field in the step's output; `Ok(None)`
/// means the step owes nothing; `Err` is the step's failure message.
pub(crate) fn accept_implementer_owed_criteria<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    input: &Value,
    output: &Value,
) -> Result<Option<Vec<OwedCriterion>>, String> {
    let owed = OwedCriterion::from_output(output).map_err(refusal)?;
    if owed.is_empty() {
        return Ok(None);
    }
    let task_id = input
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            refusal(format!(
                "`{OWED_CRITERIA_FIELD}` needs the step's `task_id`"
            ))
        })?;
    host.admit_owed_criteria(task_id, run_id, &owed)
        .map_err(|error| refusal(error.to_string()))?;
    Ok(Some(owed))
}

fn refusal(reason: String) -> String {
    format!(
        "{CONTRACT_REFUSAL_MARKER}: {reason}. Hand off a criterion as owed after deploy only \
         when the task pilot typed it post-deploy; meet every other criterion or report the \
         blocker."
    )
}
