//! Domain mutation modes must retain the managed-run dispatch denial.
use orbit_common::OrbitError;
use orbit_tools::{
    OrbitBuiltinAction, OrbitTaskScope, OrbitToolHost, ReservationOwnerContext, ToolContext,
    ToolRegistry,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct ManagedHost(AtomicUsize);
impl OrbitToolHost for ManagedHost {
    fn task_scope(&self) -> OrbitTaskScope {
        OrbitTaskScope {
            run_id: Some("jrun-managed".into()),
            ..Default::default()
        }
    }
    fn execute(
        &self,
        _: OrbitBuiltinAction,
        _: Value,
        _: Option<String>,
        _: Option<String>,
        _: Option<ReservationOwnerContext>,
    ) -> Result<Value, OrbitError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(json!({}))
    }
}
#[test]
fn domain_operator_modes_never_dispatch_from_a_managed_run() {
    let host = Arc::new(ManagedHost(AtomicUsize::new(0)));
    let ctx = ToolContext {
        orbit_host: Some(host.clone()),
        ..Default::default()
    };
    let calls: Vec<(&str, Value)> = vec![
        (
            "orbit.workflow.auto",
            json!({"workspace":"ws_fixture","action":"start","for_seconds":60}),
        ),
        (
            "orbit.routine.control",
            json!({"workspace":"ws_fixture","action":"toggle","name":"routine","target":"job:maintenance","expected_enabled":true,"enabled":false}),
        ),
        (
            "orbit.workflow.auto",
            json!({"workspace":"ws_fixture","action":"resize","concurrency":2}),
        ),
        (
            "orbit.auto_task.update",
            json!({"workspace":"ws_fixture","name":"routine","expected_enabled":true,"enabled":false}),
        ),
        (
            "orbit.auto_task.mint",
            json!({"workspace":"ws_fixture","name":"routine","acknowledge_unconditional":true}),
        ),
        (
            "orbit.pipeline.invoke",
            json!({"workspace":"ws_fixture","job_name":"maintenance","default_input":true}),
        ),
    ];
    let ordinary = registry().execute(
        "orbit.pipeline.invoke",
        &ctx,
        json!({"workspace":"ws_fixture","job_name":"delivery","input":{"task_ids":["TST-1"]}}),
    );
    assert!(
        matches!(ordinary, Err(OrbitError::CapabilityDenied(_))),
        "public input cannot bypass the managed-leaf denial"
    );
    for (tool, input) in calls {
        assert!(
            matches!(
                registry().execute(tool, &ctx, input),
                Err(OrbitError::CapabilityDenied(_))
            ),
            "{}",
            tool
        );
    }
    assert_eq!(
        host.0.load(Ordering::SeqCst),
        0,
        "refusal precedes host execution"
    );
}

#[test]
fn trusted_child_admission_preserves_internal_pipeline_invocation() {
    let host = Arc::new(ManagedHost(AtomicUsize::new(0)));
    let ctx = ToolContext {
        orbit_host: Some(host.clone()),
        reservation_owner: Some(ReservationOwnerContext {
            owner_run_id: "jrun-managed".into(),
            owner_metadata_json: Some(
                json!({"pipeline_child_admission":{"parent_run_id":"jrun-managed"}}).to_string(),
            ),
        }),
        ..Default::default()
    };
    registry()
        .execute(
            "orbit.pipeline.invoke",
            &ctx,
            json!({"job_name":"child","input":{}}),
        )
        .unwrap();
    assert_eq!(host.0.load(Ordering::SeqCst), 1);
    assert!(matches!(
        registry().execute(
            "orbit.pipeline.invoke",
            &ctx,
            json!({"workspace":"ws_fixture","job_name":"child","default_input":true})
        ),
        Err(OrbitError::CapabilityDenied(_))
    ));
    assert_eq!(host.0.load(Ordering::SeqCst), 1);
}

fn registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    registry
}
