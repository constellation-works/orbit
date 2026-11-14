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

const CREATE_FIELDS: &[&str] = &[
    "title",
    "description",
    "acceptance_criteria",
    "priority",
    "crew",
];
const EDIT_FIELDS: &[&str] = &[
    "status",
    "title",
    "description",
    "acceptance_criteria",
    "priority",
    "crew",
];
const REVIEW_FIELDS: &[&str] = &["verdict", "complete"];
const COMMENT_FIELDS: &[&str] = &["comment"];

/// A guarded write's mode: the operation kind it sends, the fields it accepts
/// beyond its envelope, and the mode name its refusal states.
struct GuardedMode {
    kind: &'static str,
    allowed: &'static [&'static str],
    mode: &'static str,
}

fn guarded_mode(input: &Value, create: bool) -> GuardedMode {
    if create {
        GuardedMode {
            kind: "create",
            allowed: CREATE_FIELDS,
            mode: "creation",
        }
    } else if input.get("verdict").is_some() {
        GuardedMode {
            kind: "review",
            allowed: REVIEW_FIELDS,
            mode: "review",
        }
    } else if input.get("comment").is_some() {
        GuardedMode {
            kind: "comment",
            allowed: COMMENT_FIELDS,
            mode: "comment",
        }
    } else {
        GuardedMode {
            kind: "edit",
            allowed: EDIT_FIELDS,
            mode: "edit",
        }
    }
}

/// The fields the registry shape-checks for a guarded write: its envelope and
/// the fields its mode accepts. Any other field is left to [`write`]'s
/// refusal, which names the selector and the accepted set (ORB-24434).
pub(crate) fn shape_checked_fields(create: bool, input: &Value) -> Vec<&'static str> {
    let envelope = if create {
        CREATE_ENVELOPE_KEYS
    } else {
        UPDATE_ENVELOPE_KEYS
    };
    envelope
        .iter()
        .chain(guarded_mode(input, create).allowed)
        .copied()
        .collect()
}

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
pub(super) fn write(ctx: &ToolContext, input: Value, create: bool) -> Result<Value, OrbitError> {
    let GuardedMode {
        kind,
        allowed,
        mode,
    } = guarded_mode(&input, create);
    let envelope = if create {
        CREATE_ENVELOPE_KEYS
    } else {
        UPDATE_ENVELOPE_KEYS
    };
    let keys = envelope.iter().chain(allowed).copied().collect::<Vec<_>>();
    // Refused before the envelope's required fields: the refusal names the
    // selector and the accepted fields, so it holds whatever else is missing.
    reject_guarded_unknown_fields(&input, &keys, create, mode, allowed)?;
    let workspace = required_string(&input, &["workspace"], "workspace")?;
    let request_id = required_string(&input, &["request_id"], "request_id")?;
    let mut operation = serde_json::Map::new();
    operation.insert("kind".into(), json!(kind));
    if !create {
        operation.insert("id".into(), json!(required_string(&input, &["id"], "id")?));
        operation.insert(
            "expected_revision".into(),
            json!(required_string(
                &input,
                &["expected_revision"],
                "expected_revision"
            )?),
        );
    }
    let values = allowed
        .iter()
        .filter_map(|key| input.get(*key).map(|value| ((*key).into(), value.clone())))
        .collect::<serde_json::Map<String, Value>>();
    if kind == "edit" {
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
