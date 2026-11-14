//! Optional guarded modes of existing task verbs, sharing the transactional write path.
use crate::{OrbitBuiltinAction, ToolContext};
use orbit_common::{
    OrbitError,
    protocol::tool_input::{reject_unknown_tool_fields, required_string},
};
use orbit_types::tool::ToolParam;
use serde_json::{Value, json};

pub(crate) fn param(name: &str, ty: &str, description: &str) -> ToolParam {
    ToolParam {
        name: name.into(),
        param_type: ty.into(),
        description: description.into(),
        required: false,
    }
}
pub(super) fn is_guarded(input: &Value) -> bool {
    ["request_id", "expected_revision", "verdict", "complete"]
        .iter()
        .any(|key| input.get(*key).is_some())
}

const GUARDED_SELECTOR_KEYS: &[&str] = &["request_id", "expected_revision", "verdict", "complete"];

const CREATE_ENVELOPE_KEYS: &[&str] = &["workspace", "request_id", "model"];
const UPDATE_ENVELOPE_KEYS: &[&str] = &[
    "workspace",
    "request_id",
    "id",
    "expected_revision",
    "model",
];

fn guarded_selectors_present(input: &Value) -> Vec<&str> {
    GUARDED_SELECTOR_KEYS
        .iter()
        .copied()
        .filter(|key| input.get(*key).is_some())
        .collect()
}

fn quote_list(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Refusal for a guarded write carrying fields outside its allowed set.
///
/// The message keeps the `unknown field(s)` prefix (and any `did you mean`
/// hint) from [`reject_unknown_tool_fields`], then names the guarded key(s)
/// that selected the guarded mode, lists the fields that mode accepts, and
/// says to drop the guarded keys for an ordinary write.
fn reject_guarded_unknown_fields(
    input: &Value,
    keys: &[&str],
    create: bool,
    mode: &str,
    accepted: &[&str],
) -> Result<(), OrbitError> {
    if let Err(previous) = reject_unknown_tool_fields(input, keys) {
        let suggestions = previous
            .did_you_mean()
            .map(|names| names.to_vec())
            .unwrap_or_default();
        let unknown = input
            .as_object()
            .map(|object| {
                object
                    .keys()
                    .filter(|key| {
                        let key = key.as_str();
                        !orbit_common::protocol::tool_input::TOOL_INPUT_TRANSPORT_WRAPPER_KEYS
                            .contains(&key)
                            && !keys.contains(&key)
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let hint = suggestions
            .first()
            .map(|name| format!(" (did you mean '{name}'?)"))
            .unwrap_or_default();
        let unknown_part = if unknown.len() == 1 {
            format!("unknown field '{}'{hint}", unknown[0])
        } else {
            format!("unknown fields '{}'{hint}", unknown.join("', '"))
        };
        let present = guarded_selectors_present(input);
        let selectors = if present.is_empty() {
            "'request_id'".to_string()
        } else {
            quote_list(&present)
        };
        let verb = if present.len() == 1 {
            "selects"
        } else {
            "select"
        };
        let noun = if create {
            "guarded creation".to_string()
        } else {
            format!("guarded update ({mode})")
        };
        let accepted_list = quote_list(accepted);
        let envelope = if create {
            quote_list(CREATE_ENVELOPE_KEYS)
        } else {
            quote_list(UPDATE_ENVELOPE_KEYS)
        };
        let remedy = if create {
            "drop 'request_id' for an ordinary create".to_string()
        } else {
            "drop 'request_id', 'expected_revision', 'verdict', 'complete' for an ordinary update"
                .to_string()
        };
        let message = format!(
            "{unknown_part}; {selectors} {verb} {noun}, which accepts only {accepted_list} (plus {envelope}); {remedy}"
        );
        return Err(OrbitError::invalid_input_with_suggestions(
            message,
            suggestions,
        ));
    }
    Ok(())
}
fn guarded_required_string(input: &Value, field: &str, create: bool) -> Result<String, OrbitError> {
    if input.get(field).is_some() {
        return required_string(input, &[field], field);
    }
    let required: &[&str] = if create {
        &[
            "workspace",
            "request_id",
            "title",
            "description",
            "acceptance_criteria",
        ]
    } else {
        &["workspace", "request_id", "id", "expected_revision"]
    };
    let missing: Vec<_> = required
        .iter()
        .copied()
        .filter(|key| input.get(*key).is_none())
        .collect();
    let selectors = guarded_selectors_present(input);
    let verb = if selectors.len() == 1 {
        "selects"
    } else {
        "select"
    };
    let mode = if create {
        "guarded creation"
    } else {
        "guarded update"
    };
    let remedy = if create {
        format!("drop {} for an ordinary create", quote_list(&selectors))
    } else {
        "read 'expected_revision' from orbit.task.show with snapshot: true; drop 'request_id', 'expected_revision', 'verdict', 'complete' for an ordinary update".to_string()
    };
    let previous = orbit_common::protocol::tool_input::missing_tool_field(input, field);
    let suggestions = previous
        .did_you_mean()
        .map(|names| names.to_vec())
        .unwrap_or_default();
    let prefix = match previous {
        OrbitError::InvalidInput(message) | OrbitError::InvalidInputDiagnostic { message, .. } => {
            message
        }
        other => other.to_string(),
    };
    let message = format!(
        "{prefix}; {} {verb} {mode}, which requires {}; missing {}; {remedy}",
        quote_list(&selectors),
        quote_list(required),
        quote_list(&missing)
    );
    Err(OrbitError::invalid_input_with_suggestions(
        message,
        suggestions,
    ))
}
pub(super) fn write(ctx: &ToolContext, input: Value, create: bool) -> Result<Value, OrbitError> {
    let workspace = guarded_required_string(&input, "workspace", create)?;
    let request_id = guarded_required_string(&input, "request_id", create)?;
    let mut operation = serde_json::Map::new();
    let (allowed, mode): (&[&str], &str) = if create {
        operation.insert("kind".into(), json!("create"));
        (
            &[
                "title",
                "description",
                "acceptance_criteria",
                "priority",
                "crew",
            ],
            "creation",
        )
    } else {
        operation.insert(
            "id".into(),
            json!(guarded_required_string(&input, "id", create)?),
        );
        operation.insert(
            "expected_revision".into(),
            json!(guarded_required_string(
                &input,
                "expected_revision",
                create
            )?),
        );
        if input.get("verdict").is_some() {
            operation.insert("kind".into(), json!("review"));
            (&["verdict", "complete"], "review")
        } else if input.get("comment").is_some() {
            operation.insert("kind".into(), json!("comment"));
            (&["comment"], "comment")
        } else {
            operation.insert("kind".into(), json!("edit"));
            (
                &[
                    "status",
                    "title",
                    "description",
                    "acceptance_criteria",
                    "priority",
                    "crew",
                ],
                "edit",
            )
        }
    };
    let mut keys = vec!["workspace", "request_id", "model"];
    if !create {
        keys.extend(["id", "expected_revision"]);
    }
    keys.extend(allowed);
    reject_guarded_unknown_fields(&input, &keys, create, mode, allowed)?;
    let values = allowed
        .iter()
        .filter_map(|key| input.get(*key).map(|value| ((*key).into(), value.clone())))
        .collect::<serde_json::Map<String, Value>>();
    if operation["kind"] == "edit" {
        operation.insert("fields".into(), Value::Object(values));
    } else {
        operation.extend(values);
    }
    let mut request =
        json!({"workspace":workspace, "request_id":request_id, "operation":operation});
    if let Some(model) = input.get("model") {
        request["model"] = model.clone();
    }
    super::super::execute_host_action(ctx, request, OrbitBuiltinAction::DesktopTaskWrite)
}

/// `orbit.task.add`'s schema extension: guarded creation's conditionals,
/// without weakening ordinary creation's requirements.
///
/// Relation types and `regression_from` attribution are not repeated on the
/// wire schema; they live in the installed Orbit skill `references/task-fields.md`.
pub(super) fn extend_create_schema(value: &mut Value) {
    value["allOf"] = json!([{
        "if":{"required":["request_id"]},
        "then":{"required":["workspace","description","acceptance_criteria"],"propertyNames":{"enum":["workspace","request_id","title","description","acceptance_criteria","priority","crew"]},"properties":{"acceptance_criteria":{"type":"array","items":{"type":"string"}},"crew":{"type":["string","null"]}}},
        "else":{"required":["description","complexity"]}
    }]);
    // Nullable crew is used only by guarded creation; ordinary creation still
    // parses and validates its existing field semantics at the host boundary.
    value["properties"]["crew"]["type"] = json!(["string", "null"]);
}

/// `orbit.task.update`'s schema extension: a review verdict is an object, and
/// guarded writes keep their conditionals. The verdict's fields are documented
/// in the installed Orbit skill `references/task-fields.md`; the host still
/// validates the object.
pub(super) fn extend_update_schema(value: &mut Value) {
    value["properties"]["verdict"] = json!({
        "type": "object",
        "description": "Review verdict object."
    });
    value["allOf"] = json!([{
        "if":{"anyOf":[{"required":["request_id"]},{"required":["expected_revision"]},{"required":["verdict"]},{"required":["complete"]}]},
        "then":{"required":["workspace","request_id","expected_revision"],"oneOf":[
            {"required":["verdict"],"propertyNames":{"enum":["workspace","request_id","id","expected_revision","verdict","complete"]}},
            {"required":["comment"],"propertyNames":{"enum":["workspace","request_id","id","expected_revision","comment"]}},
            {"propertyNames":{"enum":["workspace","request_id","id","expected_revision","status","title","description","acceptance_criteria","priority","crew"]},"properties":{"acceptance_criteria":{"type":"array","items":{"type":"string"}}}}
        ]}
    }]);
}
