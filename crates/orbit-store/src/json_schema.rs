use jsonschema::JSONSchema;
use orbit_common::OrbitError;
use serde_json::Value;

pub fn validate_schema_document(schema: &Value, context: &str) -> Result<JSONSchema, OrbitError> {
    enforce_minimum_supported_draft(schema, context)?;
    JSONSchema::options().compile(schema).map_err(|err| {
        OrbitError::SkillValidation(format!(
            "{context} must be a valid JSON Schema document: {err}"
        ))
    })
}

fn enforce_minimum_supported_draft(schema: &Value, context: &str) -> Result<(), OrbitError> {
    let Some(uri) = schema
        .as_object()
        .and_then(|obj| obj.get("$schema"))
        .and_then(Value::as_str)
    else {
        return Ok(());
    };

    let uri_lower = uri.to_ascii_lowercase();
    if uri_lower.contains("draft-03")
        || uri_lower.contains("draft-04")
        || uri_lower.contains("draft-05")
        || uri_lower.contains("draft-06")
    {
        return Err(OrbitError::SkillValidation(format!(
            "{context} declares unsupported JSON Schema draft in '$schema': {uri}"
        )));
    }
    Ok(())
}
