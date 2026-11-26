use serde_json::Value;

use crate::command::{CommandOut, Payload};

pub(super) fn render(value: &Value) -> CommandOut {
    let Some(tags) = value.as_array() else {
        return Ok(Payload::document(value.clone()).into());
    };
    let lines = tags
        .iter()
        .map(|tag| match tag {
            Value::String(name) => name.clone(),
            Value::Object(object) => {
                let name = object
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let description = object
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if description.is_empty() {
                    name.to_string()
                } else {
                    format!("{name}\t{description}")
                }
            }
            other => other.to_string(),
        })
        .collect::<Vec<_>>();
    Ok(Payload::detail(value.clone(), lines.join("\n")).into())
}
