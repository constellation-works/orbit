use serde_json::Value;

pub(super) fn text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

pub(super) fn joined(value: &Value) -> String {
    let items: Vec<String> = value.as_array().into_iter().flatten().map(text).collect();
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}
