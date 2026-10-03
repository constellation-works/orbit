//! Explicit mixed-version desktop/domain routing. Negotiation happens before dispatch;
//! lost replies never trigger a second mutation submission.
use orbit_common::{OrbitError, protocol::tool_input::reject_unknown_tool_fields};
use serde_json::{Value, json};

pub(crate) struct Translation {
    pub(crate) name: &'static str,
    pub(crate) input: Value,
    pub(crate) projection: Projection,
}
pub(crate) enum Projection {
    Identity,
    Catalog,
    LegacyCatalog,
}
impl Translation {
    fn new(name: &'static str, input: Value) -> Self {
        Self {
            name,
            input,
            projection: Projection::Identity,
        }
    }
    pub(crate) fn response(&self, value: Value) -> Value {
        match self.projection {
            Projection::Identity => value,
            Projection::Catalog => value["catalog"].clone(),
            // The legacy response is explicitly a catalog observation. No runs
            // are inferred when an older peer cannot return the combined view.
            Projection::LegacyCatalog => {
                json!({"workspace":value["workspace"],"catalog":value,"runs":null,"runs_unavailable":"older destination returns catalog separately"})
            }
        }
    }
}
fn remove(input: &mut Value, keys: &[&str]) {
    if let Some(object) = input.as_object_mut() {
        for key in keys {
            object.remove(*key);
        }
    }
}
pub(crate) fn to_domain(name: &str, mut input: Value) -> Result<Option<Translation>, OrbitError> {
    Ok(Some(match name {
        "orbit.desktop.read" => {
            let scope = input["scope"].as_str().unwrap_or_default().to_owned();
            let name = match scope.as_str() {
                "tasks" => "orbit.task.list",
                "task" => "orbit.task.show",
                "runs" | "jobs" => "orbit.workflow.run.list",
                "run" => "orbit.workflow.run.show",
                "auto_tasks" => "orbit.auto_task.list",
                "drain" => "orbit.workflow.auto",
                "routines" => "orbit.routine.control",
                _ => return Ok(None),
            };
            remove(&mut input, &["scope"]);
            input["view"] = json!("bounded");
            let mut translation = Translation::new(name, input);
            match scope.as_str() {
                "jobs" => {
                    translation.input["include_catalog"] = json!(true);
                    translation.projection = Projection::Catalog;
                }
                "drain" => {
                    remove(&mut translation.input, &["view"]);
                    translation.input["action"] = json!("status");
                }
                "routines" => {
                    remove(&mut translation.input, &["view"]);
                    translation.input["action"] = json!("list");
                }
                _ => {}
            }
            translation
        }
        "orbit.desktop.task.snapshot" => {
            input["snapshot"] = json!(true);
            Translation::new("orbit.task.show", input)
        }
        "orbit.desktop.task.write" => {
            reject_unknown_tool_fields(&input, &["workspace", "request_id", "operation", "model"])?;
            let mut operation = input["operation"].clone();
            let kind = operation["kind"].as_str().unwrap_or_default().to_owned();
            let name = if kind == "create" {
                "orbit.task.add"
            } else {
                "orbit.task.update"
            };
            let allowed = match kind.as_str() {
                "create" => vec![
                    "kind",
                    "title",
                    "description",
                    "acceptance_criteria",
                    "priority",
                    "crew",
                ],
                "edit" => vec!["kind", "id", "expected_revision", "fields"],
                "comment" => vec!["kind", "id", "expected_revision", "comment"],
                "review" => vec!["kind", "id", "expected_revision", "verdict", "complete"],
                _ => {
                    return Err(OrbitError::InvalidInput(
                        "unknown guarded task operation".into(),
                    ));
                }
            };
            reject_unknown_tool_fields(&operation, &allowed)?;
            remove(&mut input, &["operation"]);
            remove(&mut operation, &["kind"]);
            if kind == "edit" {
                let fields = operation["fields"].clone();
                reject_unknown_tool_fields(
                    &fields,
                    &[
                        "title",
                        "description",
                        "acceptance_criteria",
                        "priority",
                        "crew",
                    ],
                )?;
                remove(&mut operation, &["fields"]);
                if let Some(object) = operation.as_object_mut() {
                    if let Some(fields) = fields.as_object() {
                        object.extend(fields.clone());
                    }
                }
            }
            if let Some(object) = input.as_object_mut() {
                if let Some(operation) = operation.as_object() {
                    object.extend(operation.clone());
                }
            }
            Translation::new(name, input)
        }
        "orbit.desktop.drain" => Translation::new("orbit.workflow.auto", input),
        "orbit.desktop.automation" => {
            let kind = input["kind"].as_str().unwrap_or_default();
            let action = input["action"].as_str().unwrap_or_default();
            let name = match (kind, action) {
                ("routine", "toggle") => "orbit.routine.control",
                ("auto_task", "toggle") => "orbit.auto_task.toggle",
                ("auto_task", "mint") => "orbit.auto_task.mint",
                ("job", "run") => "orbit.pipeline.invoke",
                _ => return Ok(None),
            };
            if name == "orbit.pipeline.invoke" {
                reject_unknown_tool_fields(
                    &input,
                    &["workspace", "action", "kind", "name", "model"],
                )?;
                input["job_name"] = input["name"].clone();
                input["default_input"] = json!(true);
                remove(&mut input, &["kind", "action", "name"]);
            } else if name == "orbit.routine.control" {
                remove(&mut input, &["kind"]);
            } else {
                remove(&mut input, &["kind", "action"]);
            }
            Translation::new(name, input)
        }
        _ => return Ok(None),
    }))
}
pub(crate) fn to_legacy(name: &str, mut input: Value) -> Result<Option<Translation>, OrbitError> {
    for key in [
        "snapshot",
        "default_input",
        "include_catalog",
        "expected_enabled",
        "acknowledge_unconditional",
    ] {
        if input.get(key).is_some_and(|value| !value.is_boolean()) {
            return Err(OrbitError::InvalidInput(format!("{key} must be boolean")));
        }
    }
    if input.get("view").is_some_and(|value| value != "bounded") {
        return Err(OrbitError::InvalidInput("view must be bounded".into()));
    }
    Ok(Some(match name {
        "orbit.workflow.auto" => {
            if input["action"] == "status" {
                remove(&mut input, &["action"]);
                input["scope"] = json!("drain");
                Translation::new("orbit.desktop.read", input)
            } else {
                Translation::new("orbit.desktop.drain", input)
            }
        }
        "orbit.routine.control" => {
            if input["action"] == "list" {
                remove(&mut input, &["action"]);
                input["scope"] = json!("routines");
                Translation::new("orbit.desktop.read", input)
            } else {
                input["kind"] = json!("routine");
                Translation::new("orbit.desktop.automation", input)
            }
        }
        "orbit.task.show" if input["snapshot"] == true => {
            reject_unknown_tool_fields(&input, &["workspace", "snapshot", "id", "model"])?;
            remove(&mut input, &["snapshot"]);
            Translation::new("orbit.desktop.task.snapshot", input)
        }
        "orbit.task.add" | "orbit.task.update" if input.get("request_id").is_some() => {
            let create = name == "orbit.task.add";
            let fields = [
                "title",
                "description",
                "acceptance_criteria",
                "priority",
                "crew",
            ];
            let allowed = if create {
                vec![
                    "workspace",
                    "request_id",
                    "model",
                    "title",
                    "description",
                    "acceptance_criteria",
                    "priority",
                    "crew",
                ]
            } else if input.get("verdict").is_some() {
                vec![
                    "workspace",
                    "request_id",
                    "model",
                    "id",
                    "expected_revision",
                    "verdict",
                    "complete",
                ]
            } else if input.get("comment").is_some() {
                vec![
                    "workspace",
                    "request_id",
                    "model",
                    "id",
                    "expected_revision",
                    "comment",
                ]
            } else {
                vec![
                    "workspace",
                    "request_id",
                    "model",
                    "id",
                    "expected_revision",
                    "title",
                    "description",
                    "acceptance_criteria",
                    "priority",
                    "crew",
                ]
            };
            reject_unknown_tool_fields(&input, &allowed)?;
            let mut operation = json!({"kind":if create {"create"} else if input.get("verdict").is_some() {"review"} else if input.get("comment").is_some() {"comment"} else {"edit"}});
            if operation["kind"] == "edit" {
                operation["fields"] = json!({});
                for key in fields {
                    if let Some(value) = input.get(key) {
                        operation["fields"][key] = value.clone();
                    }
                }
            }
            for key in allowed
                .iter()
                .filter(|key| !["workspace", "request_id", "model"].contains(key))
            {
                if operation["kind"] == "edit" && fields.contains(key) {
                    continue;
                }
                if let Some(value) = input.get(*key) {
                    operation[*key] = value.clone();
                }
            }
            let mut legacy = json!({"workspace":input["workspace"],"request_id":input["request_id"],"operation":operation});
            if let Some(model) = input.get("model") {
                legacy["model"] = model.clone();
            }
            Translation::new("orbit.desktop.task.write", legacy)
        }
        "orbit.auto_task.toggle" if input.get("expected_enabled").is_some() => {
            input["kind"] = json!("auto_task");
            input["action"] = json!("toggle");
            Translation::new("orbit.desktop.automation", input)
        }
        "orbit.auto_task.mint" if input.get("acknowledge_unconditional").is_some() => {
            input["kind"] = json!("auto_task");
            input["action"] = json!("mint");
            Translation::new("orbit.desktop.automation", input)
        }
        "orbit.pipeline.invoke" if input["default_input"] == true => {
            reject_unknown_tool_fields(
                &input,
                &["workspace", "job_name", "default_input", "model"],
            )?;
            input["name"] = input["job_name"].clone();
            input["kind"] = json!("job");
            input["action"] = json!("run");
            remove(&mut input, &["job_name", "default_input"]);
            Translation::new("orbit.desktop.automation", input)
        }
        _ if input["view"] == "bounded" => {
            let allowed: &[&str] = match name {
                "orbit.task.list" => &[
                    "workspace",
                    "view",
                    "limit",
                    "offset",
                    "search",
                    "status",
                    "priority",
                    "model",
                ],
                "orbit.task.show" => &[
                    "workspace",
                    "view",
                    "id",
                    "limit",
                    "comments_offset",
                    "history_offset",
                    "artifacts_offset",
                    "model",
                ],
                "orbit.workflow.run.list" => &[
                    "workspace",
                    "view",
                    "include_catalog",
                    "limit",
                    "offset",
                    "status",
                    "model",
                ],
                "orbit.workflow.run.show" => {
                    &["workspace", "view", "id", "log_offset", "limit", "model"]
                }
                "orbit.auto_task.list" => &["workspace", "view", "offset", "limit", "model"],
                _ => return Ok(None),
            };
            reject_unknown_tool_fields(&input, allowed)?;
            let scope = match name {
                "orbit.task.list" => "tasks",
                "orbit.task.show" => "task",
                "orbit.workflow.run.list" => {
                    if input["include_catalog"] == true {
                        "jobs"
                    } else {
                        "runs"
                    }
                }
                "orbit.workflow.run.show" => "run",
                "orbit.auto_task.list" => "auto_tasks",
                _ => return Ok(None),
            };
            let catalog = scope == "jobs";
            if catalog && input.get("status").is_some() {
                return Err(OrbitError::InvalidInput(
                    "older catalog contract cannot honor a run status filter".into(),
                ));
            }
            remove(&mut input, &["view", "include_catalog"]);
            input["scope"] = json!(scope);
            let mut translation = Translation::new("orbit.desktop.read", input);
            if catalog {
                translation.projection = Projection::LegacyCatalog;
            }
            translation
        }
        _ => return Ok(None),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_translation_refuses_fields_that_cannot_be_honored() {
        for (name, input) in [
            (
                "orbit.task.list",
                json!({"view":"bounded","include_catalog":true}),
            ),
            ("orbit.task.show", json!({"view":"bounded","snapshot":true})),
            (
                "orbit.workflow.run.list",
                json!({"view":"bounded","include_catalog":"true"}),
            ),
            (
                "orbit.workflow.run.list",
                json!({"view":"bounded","include_catalog":true,"status":"running"}),
            ),
            ("orbit.pipeline.invoke", json!({"default_input":"true"})),
            ("orbit.task.show", json!({"snapshot":"true"})),
        ] {
            assert!(to_legacy(name, input).is_err(), "{name}");
        }
        assert!(
            to_legacy("orbit.task.show", json!({"snapshot":false}))
                .unwrap()
                .is_none()
        );
        assert!(
            to_legacy("orbit.pipeline.invoke", json!({"default_input":false}))
                .unwrap()
                .is_none()
        );
    }
}
