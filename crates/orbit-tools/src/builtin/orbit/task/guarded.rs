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

/// The `relations` items shape shared by `orbit.task.add` and
/// `orbit.task.update`: the `regression` classification vocabulary lives
/// here as enums and per-property descriptions, so the top-level
/// `relations` field description stays within its byte budget.
///
/// `severity_source` advertises only the filer inputs (`filer`,
/// `operator`, `backfill`): `verified` is recorded only by finding
/// verification and `default` only for a link stored without a severity,
/// so neither is a value a filer should send.
fn extend_relations_schema(value: &mut Value) {
    let Some(relations) = value
        .get_mut("properties")
        .and_then(|properties| properties.get_mut("relations"))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    relations.insert(
        "items".into(),
        json!({"type":"object","additionalProperties":false,"required":["type","target"],"properties":{
            "type":{"type":"string","enum":["blocked_by","child_of","spawned_from","regression_from","supersedes","related_to","covered_by","produces","resolves"],"description":"Relation kind; only `regression_from` may carry a `regression` object"},
            "target":{"type":"string","description":"Target task ID; `covered_by` also accepts a `github-pr:` reference and `produces`/`resolves` also accept a friction or ADR ID"},
            "regression":{"type":"object","additionalProperties":false,"required":["nature","escape"],"description":"Classification of a `regression_from` link; accepted only on that relation type","properties":{
                "nature":{"type":"string","enum":["broke_existing","defect_in_new","incomplete","merge_reintroduced","contract_drift","unclassified"],"description":"What kind of regression the link records"},
                "escape":{"type":"string","enum":["no_test","test_not_selected","platform_only","review_missed","spec_ambiguous","nondeterministic","unclassified"],"description":"Why the delivery gates did not catch the defect"},
                "severity":{"type":"string","enum":["critical","high","medium","low","nit"],"description":"How much the defect matters: critical (data loss, a security hole, every drain halted, silently wrong results), high (a core workflow broken with no workaround), medium (a feature broken in a real path, with a workaround), low (an edge case or misleading message) or nit (wording, doc drift, cosmetic)"},
                "severity_source":{"type":"string","enum":["filer","operator","backfill"],"description":"Who set the severity: filer (the default when a severity is given), operator or backfill. `verified` and `default` are recorded values that read back, not filer inputs"},
                "sites":{"type":"array","items":{"type":"string"},"maxItems":8,"uniqueItems":true,"description":"Up to 8 `symbol:` or `file:` selectors of where the defect lives, or a `dir:` selector when no narrower site can be named"}
            }}
        }}),
    );
}

/// `orbit.task.add`'s schema extension: guarded creation's conditionals,
/// without weakening ordinary creation's requirements.
pub(super) fn extend_create_schema(value: &mut Value) {
    extend_relations_schema(value);
    value["allOf"] = json!([{
        "if":{"required":["request_id"]},
        "then":{"required":["workspace","description","acceptance_criteria"],"propertyNames":{"enum":["workspace","request_id","model","title","description","acceptance_criteria","priority","crew"]},"properties":{"acceptance_criteria":{"type":"array","items":{"type":"string"}},"crew":{"type":["string","null"]}}},
        "else":{"required":["description","complexity"]}
    }]);
    // Nullable crew is used only by guarded creation; ordinary creation still
    // parses and validates its existing field semantics at the host boundary.
    value["properties"]["crew"]["type"] = json!(["string", "null"]);
}

/// `orbit.task.update`'s schema extension: the review verdict's shape and
/// the guarded write's conditionals.
pub(super) fn extend_update_schema(value: &mut Value) {
    extend_relations_schema(value);
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
