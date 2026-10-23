use serde_json::Value;

use crate::OrbitError;
use crate::protocol::tool_input::{
    optional_csv_or_string_list_alias, optional_raw_string, optional_string, required_string,
};

#[cfg(doc)]
use super::OperationSpec;

/// An integer parameter's value: a JSON number or a numeric string.
pub(super) fn non_negative_integer(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(raw) => raw.trim().parse().ok(),
        _ => None,
    }
}

/// A call's input after [`OperationSpec::parse_input`]: a JSON object keyed by
/// canonical parameter names, whose present values match their declared types.
///
/// The accessors apply the content rules every handler shared before the
/// kernel checked input: trimmed strings, non-empty unless read raw, and the
/// lenient string-list forms.
#[derive(Debug, Clone)]
pub struct OperationInput {
    pub(super) input: Value,
    pub(super) unknown_fields: Vec<String>,
}

impl OperationInput {
    /// Undeclared fields the input carried, in input order. They are reported
    /// by the tool entry and ignored by handlers.
    pub fn unknown_fields(&self) -> &[String] {
        &self.unknown_fields
    }

    /// The canonical input object.
    pub fn as_value(&self) -> &Value {
        &self.input
    }

    /// The canonical input object, for a host that takes JSON.
    pub fn into_value(self) -> Value {
        self.input
    }

    /// A trimmed, non-empty string the operation requires.
    pub fn required_string(&self, name: &str) -> Result<String, OrbitError> {
        required_string(&self.input, &[name], name)
    }

    /// A trimmed string; present but blank is refused.
    pub fn optional_string(&self, name: &str) -> Result<Option<String>, OrbitError> {
        optional_string(&self.input, name)
    }

    /// A string exactly as sent, for parameters where blank has meaning.
    pub fn optional_raw_string(&self, name: &str) -> Result<Option<String>, OrbitError> {
        optional_raw_string(&self.input, name)
    }

    /// A string list from an array, a string, a comma-separated string or a
    /// JSON-encoded array string.
    pub fn optional_string_list(&self, name: &str) -> Result<Option<Vec<String>>, OrbitError> {
        optional_csv_or_string_list_alias(&self.input, &[name])
    }

    /// A non-negative integer from a number or a numeric string.
    pub fn optional_count(&self, name: &str) -> Result<Option<usize>, OrbitError> {
        let Some(value) = self.input.get(name) else {
            return Ok(None);
        };
        non_negative_integer(value)
            .and_then(|count| usize::try_from(count).ok())
            .map(Some)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!("`{name}` must be a non-negative integer"))
            })
    }

    /// A boolean.
    pub fn optional_bool(&self, name: &str) -> Result<Option<bool>, OrbitError> {
        match self.input.get(name) {
            None => Ok(None),
            Some(Value::Bool(value)) => Ok(Some(*value)),
            Some(_) => Err(OrbitError::InvalidInput(format!(
                "`{name}` must be a boolean"
            ))),
        }
    }
}
