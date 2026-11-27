use orbit_common::OrbitError;
use serde_json::{Map, Value, json};

/// Fold the CLI-only post flags into the tool's input.
pub(crate) fn post_cli_input(input: &mut Map<String, Value>) -> Result<(), OrbitError> {
    if let Some(path) = input.remove("body_file") {
        let path = path.as_str().unwrap_or_default().to_string();
        let body = std::fs::read_to_string(&path).map_err(|error| {
            OrbitError::InvalidInput(format!("cannot read --body-file {path}: {error}"))
        })?;
        input.insert("body".into(), Value::String(body));
    }
    if input
        .remove("keep_after_done")
        .and_then(|value| value.as_bool())
        == Some(true)
    {
        input.insert("close_on_linked_done".into(), Value::Bool(false));
    }
    if let Some(refs) = input.remove("evidence_refs") {
        let mut evidence = Vec::new();
        for item in refs.as_array().into_iter().flatten() {
            let raw = item.as_str().unwrap_or_default();
            let Some((kind, value)) = raw.split_once('=') else {
                return Err(OrbitError::InvalidInput(format!(
                    "--evidence takes TYPE=VALUE, got `{raw}`"
                )));
            };
            evidence.push(json!({"type": kind.trim(), "value": value.trim()}));
        }
        input.insert("evidence".into(), Value::Array(evidence));
    }
    Ok(())
}
