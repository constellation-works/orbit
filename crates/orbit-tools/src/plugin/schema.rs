//! Bridging a plugin tool's JSON Schema and the registry's flat
//! [`ToolParam`] list, plus the schema a plugin tool is loaded with. The
//! registry, `orbit tool show` and the MCP adapter all speak parameters; a
//! manifest speaks JSON Schema.

use std::sync::Arc;

use orbit_common::protocol::tool_schema::tool_input_schema_for;
use orbit_types::tool::ToolParam;
use serde_json::{Map, Value};

/// A tool schema and the validator compiled from it, once, at load.
///
/// Compiling here is what makes a nested unresolvable `$ref` or an invalid
/// keyword refuse *that plugin* while it is being read (§4.9) instead of
/// failing every call it is ever asked to check, and it keeps schema
/// compilation off the per-call path.
#[derive(Clone)]
pub struct CompiledSchema {
    schema: Value,
    validator: Arc<jsonschema::JSONSchema>,
}

impl CompiledSchema {
    /// Check every `$ref` and compile `schema`, describing the first problem.
    pub fn compile(schema: Value) -> Result<Self, String> {
        refuse_unresolvable_refs(&schema, &schema)?;
        let validator = jsonschema::JSONSchema::compile(&schema)
            .map_err(|error| format!("the schema does not compile: {error}"))?;
        Ok(Self {
            schema,
            validator: Arc::new(validator),
        })
    }

    /// The document this validator was compiled from.
    pub fn schema(&self) -> &Value {
        &self.schema
    }

    /// Every way `instance` violates the schema, as one diagnostic; `None`
    /// when it validates.
    pub fn violations(&self, instance: &Value) -> Option<String> {
        let errors = self.validator.validate(instance).err()?;
        Some(
            errors
                .map(|error| {
                    let path = error.instance_path.to_string();
                    if path.is_empty() {
                        error.to_string()
                    } else {
                        format!("{path}: {error}")
                    }
                })
                .collect::<Vec<_>>()
                .join("; "),
        )
    }
}

/// Keywords whose value is instance data rather than a subschema: a `$ref`
/// key inside one of them is a plain object the tool exchanges, not a
/// reference this walk may read.
const SCHEMA_DATA_KEYWORDS: &[&str] = &["const", "enum", "default", "examples"];

/// Refuse a `$ref` that resolves to nothing, at any depth.
///
/// The library compiles an unresolvable pointer without complaint and then
/// fails every instance it is asked to check, which is the failure §4.9
/// moves to load. A schema document may only reference itself: the manifest's
/// own `{ $ref: <path> }` form is resolved against the plugin root before
/// this runs, and nothing here can fetch a file or a URL.
fn refuse_unresolvable_refs(root: &Value, node: &Value) -> Result<(), String> {
    match node {
        Value::Object(fields) => {
            if let Some(reference) = fields.get("$ref").and_then(Value::as_str) {
                let Some(pointer) = reference.strip_prefix('#') else {
                    return Err(format!(
                        "the schema has a $ref to '{reference}'; a tool schema may only \
                         reference its own document with a '#/…' pointer"
                    ));
                };
                if !pointer.is_empty() && root.pointer(pointer).is_none() {
                    return Err(format!(
                        "the schema has a $ref to '{reference}', which resolves to nothing \
                         inside it"
                    ));
                }
            }
            for (key, value) in fields {
                if SCHEMA_DATA_KEYWORDS.contains(&key.as_str()) {
                    continue;
                }
                refuse_unresolvable_refs(root, value)?;
            }
            Ok(())
        }
        Value::Array(items) => items
            .iter()
            .try_for_each(|item| refuse_unresolvable_refs(root, item)),
        _ => Ok(()),
    }
}

impl std::fmt::Debug for CompiledSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledSchema")
            .field("schema", &self.schema)
            .finish_non_exhaustive()
    }
}

/// The validator is a pure function of the document, so comparing documents
/// compares what two compiled schemas accept.
impl PartialEq for CompiledSchema {
    fn eq(&self, other: &Self) -> bool {
        self.schema == other.schema
    }
}

/// Flatten the top-level `properties` of an object schema into parameters.
/// Nested shapes are kept as their outer type; `orbit tool run --input`
/// still accepts the full payload.
pub fn params_from_input_schema(schema: &Value) -> Vec<ToolParam> {
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    properties
        .iter()
        .map(|(name, property)| ToolParam {
            name: name.clone(),
            description: property
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            param_type: param_type_for(property),
            required: required.iter().any(|entry| entry == name),
        })
        .collect()
}

fn param_type_for(property: &Value) -> String {
    if let Some(kind) = property.get("type").and_then(Value::as_str) {
        return match kind {
            "string" | "integer" | "number" | "boolean" | "array" | "object" => kind.to_string(),
            _ => "string".to_string(),
        };
    }
    let alternatives = property
        .get("anyOf")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    let kind = entry.get("type").and_then(Value::as_str).unwrap_or("");
                    let items = entry
                        .get("items")
                        .and_then(|items| items.get("type"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    (kind, items)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    match alternatives.as_slice() {
        [("array", "string"), ("string", _)] => "string_list".to_string(),
        [("object", _), ("array", "object")] => "object".to_string(),
        [("array", "object"), ("string", _)] => "object_list".to_string(),
        _ => "string".to_string(),
    }
}

/// The canonical object schema for a parameter list, as a manifest
/// `input_schema` value.
pub fn input_schema_from_params(tool_name: &str, params: &[ToolParam]) -> Value {
    let mut schema: Map<String, Value> = tool_input_schema_for(tool_name, params);
    // A manifest schema describes the plugin's contract, not the transport's
    // extra-key policy; the adapter re-applies that when it advertises.
    schema.remove("additionalProperties");
    Value::Object(schema)
}
