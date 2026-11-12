pub mod tool_input;
pub mod tool_schema;
pub mod yaml;

use serde_json::Value;

/// Recursively reorder `value` with object keys in lexicographic order.
///
/// Array order is preserved and scalar values are cloned unchanged. Keys
/// are sorted explicitly before rebuilding each map, so the result does
/// not depend on `serde_json`'s `preserve_order` feature or on how the
/// input value was constructed.
pub fn sorted_keys_json_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut sorted = serde_json::Map::new();
            for key in keys {
                sorted.insert(key.clone(), sorted_keys_json_value(&map[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted_keys_json_value).collect()),
        other => other.clone(),
    }
}

/// Compact JSON rendering of [`sorted_keys_json_value`].
pub fn sorted_keys_json_string(value: &Value) -> String {
    sorted_keys_json_value(value).to_string()
}

/// JSON string label of a serde value.
///
/// `Some` is the owned string when serialization succeeds and the JSON value
/// is a string. `None` is a serialization failure or any other JSON type.
/// The caller chooses the fallback.
pub fn serde_string_label<T: serde::Serialize + ?Sized>(value: &T) -> Option<String> {
    serde_json::to_value(value)
        .ok()
        .and_then(|json| json.as_str().map(str::to_owned))
}
