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
pub(super) fn write(ctx: &ToolContext, input: Value, create: bool) -> Result<Value, OrbitError> {
    let workspace = required_string(&input, &["workspace"], "workspace")?;
    let request_id = required_string(&input, &["request_id"], "request_id")?;
    let mut operation = serde_json::Map::new();
    let allowed: &[&str] = if create {
        operation.insert("kind".into(), json!("create"));
        &[
            "title",
            "description",
            "acceptance_criteria",
            "priority",
            "crew",
        ]
    } else {
        operation.insert("id".into(), json!(required_string(&input, &["id"], "id")?));
        operation.insert(
            "expected_revision".into(),
            json!(required_string(
                &input,
                &["expected_revision"],
                "expected_revision"
            )?),
        );
        if input.get("verdict").is_some() {
            operation.insert("kind".into(), json!("review"));
            &["verdict", "complete"]
        } else if input.get("comment").is_some() {
            operation.insert("kind".into(), json!("comment"));
            &["comment"]
        } else {
            operation.insert("kind".into(), json!("edit"));
            &[
                "status",
                "title",
                "description",
                "acceptance_criteria",
                "priority",
                "crew",
            ]
        }
    };
    let mut keys = vec!["workspace", "request_id", "model"];
    if !create {
        keys.extend(["id", "expected_revision"]);
    }
    keys.extend(allowed);
    reject_unknown_tool_fields(&input, &keys)?;
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

/// Describe guarded extensions without weakening ordinary creation requirements.
pub(super) fn input_schema(schema: &orbit_types::tool::ToolSchema, create: bool) -> Value {
    let mut value = Value::Object(orbit_common::protocol::tool_schema::tool_input_schema(
        schema,
    ));
    if create {
        value["allOf"] = json!([{
            "if":{"required":["request_id"]},
            "then":{"required":["workspace","acceptance_criteria"],"propertyNames":{"enum":["workspace","request_id","model","title","description","acceptance_criteria","priority","crew"]},"properties":{"acceptance_criteria":{"type":"array","items":{"type":"string"}},"crew":{"type":["string","null"]}}},
            "else":{"required":["complexity"]}
        }]);
        // Nullable crew is used only by guarded creation; ordinary creation still
        // parses and validates its existing field semantics at the host boundary.
        value["properties"]["crew"]["type"] = json!(["string", "null"]);
    } else {
        let string = json!({"type":"string"});
        let strings = json!({"type":"array","items":{"type":"string"}});
        value["properties"]["verdict"] = json!({"type":"object","additionalProperties":false,"required":["decision","rationale","criteria","evidence"],"properties":{
            "decision":{"enum":["accept","changes_requested"]},"rationale":string,"evidence":strings,
            "expected_run_id":{"type":["string","null"]},"expected_head":{"type":["string","null"]},
            "criteria":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["criterion","met","evidence"],"properties":{"criterion":string,"met":{"type":"boolean"},"evidence":strings}}}
        }});
        value["allOf"] = json!([{
            "if":{"anyOf":[{"required":["request_id"]},{"required":["expected_revision"]},{"required":["verdict"]},{"required":["complete"]}]},
            "then":{"required":["workspace","request_id","expected_revision"],"oneOf":[
                {"required":["verdict"],"propertyNames":{"enum":["workspace","request_id","model","id","expected_revision","verdict","complete"]}},
                {"required":["comment"],"propertyNames":{"enum":["workspace","request_id","model","id","expected_revision","comment"]}},
                {"propertyNames":{"enum":["workspace","request_id","model","id","expected_revision","status","title","description","acceptance_criteria","priority","crew"]},"properties":{"acceptance_criteria":{"type":"array","items":{"type":"string"}}}}
            ]}
        }]);
    }
    value
}
