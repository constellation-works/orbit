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

#[derive(Debug, Clone)]
struct FakeTask {
    id: String,
    source_task_id: Option<String>,
    updated_at: String,
    history: Vec<Value>,
}

struct FakeTaskHost {
    task: Mutex<FakeTask>,
}

impl FakeTaskHost {
    fn seeded(source_task_id: Option<&str>) -> Self {
        Self {
            task: Mutex::new(FakeTask {
                id: "ORB-00001".to_string(),
                source_task_id: source_task_id.map(ToOwned::to_owned),
                updated_at: "2026-05-17T00:00:00Z".to_string(),
                history: Vec::new(),
            }),
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
        let id = input.get("id").and_then(Value::as_str).expect("id");
        let mut task = self.task.lock().expect("task lock");
        assert_eq!(id, task.id);

        if let Some(value) = input.get("source_task_id") {
            let raw = value.as_str().ok_or_else(|| {
                OrbitError::InvalidInput("`source_task_id` must be a string".to_string())
            })?;
            let next_source_task_id = (!raw.is_empty()).then(|| raw.to_string());
            if task.source_task_id != next_source_task_id {
                task.updated_at = "2026-05-17T00:00:01Z".to_string();
                task.history.push(json!({
                    "event": "updated",
                    "note": "source_task_id changed",
                }));
            }
            task.source_task_id = next_source_task_id;
        }

        Ok(json!({
            "id": task.id.clone(),
            "type": "bug",
            "source_task_id": task.source_task_id.clone(),
            "updated_at": task.updated_at.clone(),
            "history": task.history.clone(),
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
    assert!(
        param
            .description
            .contains("comma-separated string or array")
    );
    assert!(param.description.contains("file:path"));
    assert!(
        param.description.contains("filesystem anchor only"),
        "context_files help must document that a `symbol:` name is not verified: {}",
        param.description
    );
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
    assert!(param.description.contains("originating task ID"));
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
    assert!(param.description.contains("low, medium, or hard"));
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
                &update_tool_context(Arc::new(FakeTaskHost::seeded(None))),
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
            &update_tool_context(Arc::new(FakeTaskHost::seeded(None))),
            json!({
                "id": "ORB-00001",
                "model": "codex",
                "artifacts": [{"path": "report.txt", "content": [111, 107]}],
            }),
        )
        .expect_err("inline artifacts must use the bounded artifact.put surface");
    assert!(error.to_string().contains("orbit.task.artifact.put"));
}

#[test]
fn update_handler_persists_source_task_id() {
    let host = Arc::new(FakeTaskHost::seeded(None));
    let output = OrbitTaskUpdateTool
        .execute(
            &update_tool_context(Arc::clone(&host)),
            json!({
                "id": "ORB-00001",
                "model": "codex",
                "source_task_id": "ORB-00000",
            }),
        )
        .expect("update succeeds");

    assert_eq!(output.get("type").and_then(Value::as_str), Some("bug"));
    assert_eq!(
        output.get("source_task_id").and_then(Value::as_str),
        Some("ORB-00000")
    );
    assert_eq!(
        host.task
            .lock()
            .expect("task lock")
            .source_task_id
            .as_deref(),
        Some("ORB-00000")
    );
}
