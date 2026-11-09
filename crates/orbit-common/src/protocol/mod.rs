pub mod tool_input;
pub mod tool_schema;
pub mod yaml;

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
