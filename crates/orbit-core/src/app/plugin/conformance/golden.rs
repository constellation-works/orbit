use std::path::Path;

use serde_json::Value;

pub(super) fn render_golden_value(value: &Value, workspace: &Path, plugin_root: &Path) -> Value {
    match value {
        Value::String(text) => Value::String(
            text.replace("{{workspace}}", &workspace.to_string_lossy())
                .replace("{{plugin_root}}", &plugin_root.to_string_lossy()),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| render_golden_value(item, workspace, plugin_root))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        render_golden_value(value, workspace, plugin_root),
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(super) fn template_golden_value(value: &Value, workspace: &Path, plugin_root: &Path) -> Value {
    match value {
        Value::String(text) => {
            let plugin_root = plugin_root.to_string_lossy();
            let workspace = workspace.to_string_lossy();
            Value::String(
                text.replace(plugin_root.as_ref(), "{{plugin_root}}")
                    .replace(workspace.as_ref(), "{{workspace}}"),
            )
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| template_golden_value(item, workspace, plugin_root))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        template_golden_value(value, workspace, plugin_root),
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(super) fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}
