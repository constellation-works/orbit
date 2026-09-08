use serde_json::{Value, json};

pub(super) fn executor_def_json(def: &orbit_core::ExecutorDef) -> Value {
    json!({
        "name": def.name,
        "executor_type": def.executor_type.to_string(),
        "command": def.command,
        "args": def.args,
        "stdout_format": def.stdout_format.as_ref().map(ToString::to_string),
        "timeout_seconds": def.timeout_seconds,
        "env": def.env,
        "sandbox": def.sandbox,
        "allow_fallback": def.allow_fallback,
        "created_at": def.created_at.map(|t| t.to_rfc3339()),
        "updated_at": def.updated_at.map(|t| t.to_rfc3339()),
    })
}
