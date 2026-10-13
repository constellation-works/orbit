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
