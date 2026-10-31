//! Helpers the task subcommands share with the other surfaces that serve
//! `orbit.task.show`.

use orbit_cmd::task_owner::WorkspaceIdentity;
use orbit_core::OrbitError;
use serde_json::{Value, json};

/// Name the owning workspace on an unprojected `orbit.task.show` result.
///
/// Human `orbit task show`, `orbit tool run orbit.task.show`, and MCP
/// `orbit_task_show` share this so an id-only read reports the logical
/// registry identity a later write can address [ORB-10797] [ORB-10961].
pub(crate) fn attach_bound_workspace_identity(
    tool_name: &str,
    input: &Value,
    owner: Option<&WorkspaceIdentity>,
    mut output: Value,
) -> Result<Value, OrbitError> {
    if tool_name != "orbit.task.show" {
        return Ok(output);
    }
    if input.get("field").is_some() || input.get("fields").is_some() {
        return Ok(output);
    }
    // A routed worker read (`_worker_read`) answers a claimed leaf's owner
    // coordination with the typed value it asked for — history, comments, a
    // task list — not a task record, and the reader deserializes exactly that
    // shape [ORB-13625]. Decorating it would break every non-object answer.
    if input.get("_worker_read").is_some() {
        return Ok(output);
    }
    if let Some(owner) = owner {
        insert_workspace_identity(&mut output, owner)?;
    }
    Ok(output)
}

/// Name the owning workspace in the machine-readable projection, in the same
/// `(name, logical id)` shape the human line prints, so a follow-up write can
/// address it with `--workspace`.
fn insert_workspace_identity(
    value: &mut Value,
    owner: &WorkspaceIdentity,
) -> Result<(), OrbitError> {
    let object = value.as_object_mut().ok_or_else(|| {
        OrbitError::Execution("task JSON projection did not produce an object".to_string())
    })?;
    object.insert(
        "workspace".to_string(),
        json!({ "id": owner.id, "name": owner.name }),
    );
    Ok(())
}

/// Shape `--not-before` and `--clear-not-before` into the tool's `not_before`
/// field. The tool takes an RFC 3339 instant, or an empty string that clears
/// it (an explicit null is an omitted field on the wire); the CLI also takes a
/// local `YYYY-MM-DDTHH:MM`, converted here with this host's time zone. Any
/// other text is passed through for the tool to refuse, naming it.
pub(crate) fn shape_not_before_flags(
    input: &mut serde_json::Map<String, Value>,
) -> Result<(), OrbitError> {
    let clear = input.shift_remove("clear_not_before") == Some(Value::Bool(true));
    if clear {
        if input.contains_key("not_before") {
            return Err(OrbitError::InvalidInput(
                "pass either `--not-before` or `--clear-not-before`, not both".to_string(),
            ));
        }
        input.insert("not_before".to_string(), Value::String(String::new()));
        return Ok(());
    }
    let Some(Value::String(raw)) = input.get("not_before") else {
        return Ok(());
    };
    if let Some(instant) = local_not_before(raw)? {
        input.insert("not_before".to_string(), json!(instant));
    }
    Ok(())
}

/// A local `YYYY-MM-DDTHH:MM` (or with `:SS`) as an RFC 3339 UTC instant,
/// or `None` when `raw` is not in that form.
fn local_not_before(raw: &str) -> Result<Option<String>, OrbitError> {
    use chrono::{LocalResult, NaiveDateTime, TimeZone};
    let trimmed = raw.trim();
    let Some(naive) = ["%Y-%m-%dT%H:%M", "%Y-%m-%dT%H:%M:%S"]
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(trimmed, format).ok())
    else {
        return Ok(None);
    };
    let local = match chrono::Local.from_local_datetime(&naive) {
        LocalResult::Single(local) => local,
        // A wall-clock time a DST change repeats: the earlier one.
        LocalResult::Ambiguous(earlier, _) => earlier,
        LocalResult::None => {
            return Err(OrbitError::InvalidInput(format!(
                "`--not-before {trimmed}` names a local time this host's time zone skips (a \
                 daylight-saving change); pick another time or give an RFC 3339 instant with \
                 an offset"
            )));
        }
    };
    Ok(Some(orbit_types::task::format_not_before(
        local.with_timezone(&chrono::Utc),
    )))
}

/// The line `orbit task add|update` prints for a scheduled task: the time in
/// UTC and in this host's time zone.
pub(crate) fn not_before_line(task: &orbit_core::Task) -> Option<String> {
    let not_before = task.not_before?;
    let local = not_before.with_timezone(&chrono::Local);
    Some(format!(
        "Not before: {} (local {})",
        orbit_types::task::format_not_before(not_before),
        local.format("%Y-%m-%d %H:%M:%S %:z")
    ))
}
