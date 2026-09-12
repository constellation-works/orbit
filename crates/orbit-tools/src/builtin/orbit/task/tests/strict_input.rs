use std::sync::Arc;

use serde_json::{Value, json};

use orbit_common::OrbitError;

use super::super::add::OrbitTaskAddTool;
use super::super::approve::OrbitTaskApproveTool;
use super::super::start::OrbitTaskStartTool;
use super::super::update::OrbitTaskUpdateTool;
use crate::{OrbitBuiltinAction, OrbitTaskScope, OrbitToolHost, Tool, ToolContext};

struct RecordingHost;

impl OrbitToolHost for RecordingHost {
    fn execute(
        &self,
        _action: OrbitBuiltinAction,
        _input: Value,
        _agent: Option<String>,
        _model: Option<String>,
        _reservation_owner: Option<crate::ReservationOwnerContext>,
    ) -> Result<Value, OrbitError> {
        panic!("host must not run when input is invalid");
    }

    fn task_scope(&self) -> OrbitTaskScope {
        OrbitTaskScope::default()
    }
}

fn ctx() -> ToolContext {
    ToolContext {
        orbit_host: Some(Arc::new(RecordingHost)),
        ..ToolContext::default()
    }
}

fn assert_unknown_field(error: OrbitError, field: &str, hint: Option<&str>) {
    let message = error.to_string();
    assert!(
        message.contains(&format!("unknown field '{field}'")),
        "{message}"
    );
    if let Some(hint) = hint {
        assert!(
            message.contains(&format!("did you mean '{hint}'")),
            "{message}"
        );
        assert!(
            error
                .did_you_mean()
                .is_some_and(|names| names.iter().any(|name| name == hint)),
            "{error:?}"
        );
    }
}

#[test]
fn update_rejects_note_with_comment_hint() {
    let error = OrbitTaskUpdateTool
        .execute(
            &ctx(),
            json!({
                "id": "ORB-00001",
                "status": "rejected",
                "note": "should be comment",
                "model": "codex",
            }),
        )
        .expect_err("note is not an update argument");
    assert_unknown_field(error, "note", Some("comment"));
}

#[test]
fn update_rejects_bogus_key() {
    let error = OrbitTaskUpdateTool
        .execute(
            &ctx(),
            json!({
                "id": "ORB-00001",
                "bogus_key": 1,
                "model": "codex",
            }),
        )
        .expect_err("undeclared update keys must fail");
    assert_unknown_field(error, "bogus_key", None);
}

#[test]
fn add_rejects_unknown_key() {
    let error = OrbitTaskAddTool
        .execute(
            &ctx(),
            json!({
                "title": "Unknown key",
                "description": "must fail",
                "complexity": "low",
                "workspace": "/tmp/test-ws",
                "bogus_key": true,
            }),
        )
        .expect_err("undeclared add keys must fail");
    assert_unknown_field(error, "bogus_key", None);
}

#[test]
fn approve_rejects_unknown_key() {
    let error = OrbitTaskApproveTool
        .execute(
            &ctx(),
            json!({
                "id": "ORB-00001",
                "bogus_key": 1,
            }),
        )
        .expect_err("undeclared approve keys must fail");
    assert_unknown_field(error, "bogus_key", None);
}

#[test]
fn start_rejects_unknown_key() {
    let error = OrbitTaskStartTool
        .execute(
            &ctx(),
            json!({
                "id": "ORB-00001",
                "bogus_key": 1,
                "model": "codex",
            }),
        )
        .expect_err("undeclared start keys must fail");
    assert_unknown_field(error, "bogus_key", None);
}

#[test]
fn update_allows_workspace_and_meta_wrappers() {
    struct OkHost;
    impl OrbitToolHost for OkHost {
        fn execute(
            &self,
            action: OrbitBuiltinAction,
            input: Value,
            _agent: Option<String>,
            _model: Option<String>,
            _reservation_owner: Option<crate::ReservationOwnerContext>,
        ) -> Result<Value, OrbitError> {
            assert_eq!(action, OrbitBuiltinAction::TaskUpdate);
            assert_eq!(input["id"], "ORB-00001");
            Ok(json!({ "id": "ORB-00001" }))
        }

        fn task_scope(&self) -> OrbitTaskScope {
            OrbitTaskScope::default()
        }
    }

    let ctx = ToolContext {
        orbit_host: Some(Arc::new(OkHost)),
        ..ToolContext::default()
    };
    OrbitTaskUpdateTool
        .execute(
            &ctx,
            json!({
                "id": "ORB-00001",
                "workspace": "ws_orbit",
                "_meta": { "orbit": { "workspace": "ws_orbit" } },
                "model": "codex",
            }),
        )
        .expect("transport wrappers must not fail closed tools");
}
