use orbit_common::OrbitError;
use serde_json::json;
use std::sync::Arc;

use super::*;
use crate::{OrbitTaskScope, OrbitToolHost, ReservationOwnerContext};

struct ManagedHost;

impl OrbitToolHost for ManagedHost {
    fn execute(
        &self,
        _action: OrbitBuiltinAction,
        _input: serde_json::Value,
        _agent: Option<String>,
        _model: Option<String>,
        _reservation_owner: Option<ReservationOwnerContext>,
    ) -> Result<serde_json::Value, OrbitError> {
        Ok(json!({"observed": true}))
    }

    fn task_scope(&self) -> OrbitTaskScope {
        OrbitTaskScope {
            run_id: Some("jrun-managed".to_string()),
            ..OrbitTaskScope::default()
        }
    }
}

fn managed_context() -> ToolContext {
    ToolContext {
        orbit_host: Some(Arc::new(ManagedHost)),
        ..ToolContext::default()
    }
}

#[test]
fn managed_run_rejects_dispatch_before_host_resolution() {
    let context = managed_context();

    let error = OrbitWorkflowShipTool
        .execute(&context, json!({"task_ids": ["ORB-00001"]}))
        .expect_err("managed run must not recursively dispatch");

    assert!(
        matches!(error, OrbitError::CapabilityDenied(_)),
        "{error:?}"
    );
    assert!(error.to_string().contains("managed runs cannot dispatch"));
}

#[test]
fn ship_schema_is_review_only_and_has_no_retired_review_controls() {
    let schema = OrbitWorkflowShipTool.schema();
    let names = schema
        .parameters
        .iter()
        .map(|parameter| parameter.name.as_str())
        .collect::<Vec<_>>();

    assert!(!names.contains(&"review"));
    assert!(!names.contains(&"review_crew"));
    assert!(!names.contains(&"completion"));
    assert!(schema.description.contains("review-only"));
    assert!(schema.description.contains("does not accept completion"));
    assert!(
        schema
            .description
            .contains("orbit run ship <task-id> --complete")
    );
}

#[test]
fn observation_remains_non_mutating_inside_managed_run() {
    let context = managed_context();

    let output = OrbitWorkflowRunShowTool
        .execute(&context, json!({"id": "jrun-example"}))
        .expect("observation remains available");

    assert_eq!(output, json!({"observed": true}));
}
