//! Tests for context_files and source_task_id exposure/handling in task.update.
//
// Migrated from nested `task/update/tests/{context_files,source_task_id}.rs` (anti-pattern)
// to single sibling `task/tests/update.rs` per ORB-00243 and
// docs/design-patterns/test_layout.md.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use orbit_common::OrbitError;

use super::super::update::*;
use crate::{OrbitBuiltinAction, OrbitTaskScope, OrbitToolHost, Tool, ToolContext};

struct FakeTaskHost {
    last_input: Mutex<Option<Value>>,
}

impl FakeTaskHost {
    fn new() -> Self {
        Self {
            last_input: Mutex::new(None),
        }
    }
}

impl OrbitToolHost for FakeTaskHost {
    fn execute(
        &self,
        action: OrbitBuiltinAction,
        input: Value,
        _agent: Option<String>,
        _model: Option<String>,
        _reservation_owner: Option<crate::ReservationOwnerContext>,
    ) -> Result<Value, OrbitError> {
        assert_eq!(action, OrbitBuiltinAction::TaskUpdate);
        let id = input
            .get("id")
            .and_then(Value::as_str)
            .expect("id")
            .to_string();
        *self.last_input.lock().expect("host input lock") = Some(input);

        Ok(json!({
            "id": id,
            "type": "bug",
        }))
    }

    fn task_scope(&self) -> OrbitTaskScope {
        OrbitTaskScope::default()
    }
}

fn update_tool_context(host: Arc<FakeTaskHost>) -> ToolContext {
    ToolContext {
        orbit_host: Some(host),
        ..ToolContext::default()
    }
}

#[test]
fn schema_exposes_context_files() {
    let schema = OrbitTaskUpdateTool.schema();

    let param = schema
        .parameters
        .iter()
        .find(|param| param.name == "context_files")
        .expect("context_files param");

    assert_eq!(param.param_type, "string_list");
    assert!(!param.required);
}

#[test]
fn schema_exposes_source_task_id() {
    let schema = OrbitTaskUpdateTool.schema();

    let param = schema
        .parameters
        .iter()
        .find(|param| param.name == "source_task_id")
        .expect("source_task_id param");

    assert_eq!(param.param_type, "string");
    assert!(!param.required);
}

#[test]
fn schema_exposes_complexity() {
    let schema = OrbitTaskUpdateTool.schema();

    let param = schema
        .parameters
        .iter()
        .find(|param| param.name == "complexity")
        .expect("complexity param");

    assert_eq!(param.param_type, "string");
    assert!(!param.required);
}

#[test]
fn schema_exposes_optional_note() {
    let schema = OrbitTaskUpdateTool.schema();
    let note = schema
        .parameters
        .iter()
        .find(|param| param.name == "note")
        .expect("note param");

    assert_eq!(note.param_type, "string");
    assert!(!note.required);
}

#[test]
fn schema_omits_and_handler_rejects_required_tools() {
    let schema = OrbitTaskUpdateTool.schema();
    assert!(
        schema
            .parameters
            .iter()
            .all(|parameter| parameter.name != "required_tools")
    );

    for field in ["required_tools", "requiredTools", "required-tool"] {
        let error = OrbitTaskUpdateTool
            .execute(
                &update_tool_context(Arc::new(FakeTaskHost::new())),
                json!({
                    "id": "ORB-00001",
                    "model": "codex",
                    field: ["proc.spawn"],
                }),
            )
            .expect_err("task required tools are creation-only");
        assert!(error.to_string().contains("immutable"), "{error}");
    }
}

#[test]
fn schema_and_handler_exclude_inline_artifacts() {
    let schema = OrbitTaskUpdateTool.schema();
    assert!(
        schema
            .parameters
            .iter()
            .all(|parameter| parameter.name != "artifacts")
    );

    let error = OrbitTaskUpdateTool
        .execute(
            &update_tool_context(Arc::new(FakeTaskHost::new())),
            json!({
                "id": "ORB-00001",
                "model": "codex",
                "artifacts": [{"path": "report.txt", "content": [111, 107]}],
            }),
        )
        .expect_err("inline artifacts must use the bounded artifact.put surface");
    assert!(error.to_string().contains("orbit.task.artifact.put"));
}

/// [ORB-12245] The lifecycle override is a human CLI action. The agent-facing
/// schema does not advertise it, and the handler refuses it rather than
/// silently ignoring it.
#[test]
fn schema_omits_and_handler_rejects_force() {
    let schema = OrbitTaskUpdateTool.schema();
    assert!(
        schema
            .parameters
            .iter()
            .all(|parameter| parameter.name != "force")
    );

    let error = OrbitTaskUpdateTool
        .execute(
            &update_tool_context(Arc::new(FakeTaskHost::new())),
            json!({
                "id": "ORB-00001",
                "model": "codex",
                "status": "done",
                "force": true,
            }),
        )
        .expect_err("agents cannot override the task lifecycle");
    assert!(
        error.to_string().contains("does not accept `force`"),
        "{error}"
    );
}

#[test]
fn update_handler_forwards_source_task_id_to_host() {
    let host = Arc::new(FakeTaskHost::new());
    OrbitTaskUpdateTool
        .execute(
            &update_tool_context(Arc::clone(&host)),
            json!({
                "id": "ORB-00001",
                "model": "codex",
                "source_task_id": "ORB-00000",
            }),
        )
        .expect("update succeeds");

    let last_input = host.last_input.lock().expect("host input lock");
    assert_eq!(
        last_input
            .as_ref()
            .and_then(|input| input.get("source_task_id"))
            .and_then(Value::as_str),
        Some("ORB-00000")
    );
}
