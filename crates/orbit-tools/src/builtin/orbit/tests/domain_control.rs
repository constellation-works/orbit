//! Domain mutation modes must retain the managed-run dispatch denial.
use crate::{
    OrbitBuiltinAction, OrbitTaskScope, OrbitToolHost, ReservationOwnerContext, Tool, ToolContext,
};
use orbit_common::OrbitError;
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
    let calls: Vec<(Box<dyn Tool>, Value)> = vec![
        (
            Box::new(super::super::domain_control::WorkflowAutoTool),
            json!({"workspace":"ws_fixture","action":"start","for_seconds":60}),
        ),
        (
            Box::new(super::super::domain_control::RoutineControlTool),
            json!({"workspace":"ws_fixture","action":"toggle","name":"routine","target":"job:maintenance","expected_enabled":true,"enabled":false}),
        ),
        (
            Box::new(super::super::auto_task::toggle::OrbitAutoTaskToggleTool),
            json!({"workspace":"ws_fixture","name":"routine","expected_enabled":true,"enabled":false}),
        ),
        (
            Box::new(super::super::auto_task::mint::OrbitAutoTaskMintTool),
            json!({"workspace":"ws_fixture","name":"routine","acknowledge_unconditional":true}),
        ),
        (
            Box::new(super::super::pipeline::invoke::OrbitPipelineInvokeTool),
            json!({"workspace":"ws_fixture","job_name":"maintenance","default_input":true}),
        ),
    ];
    let ordinary = super::super::pipeline::invoke::OrbitPipelineInvokeTool.execute(
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
                tool.execute(&ctx, input),
                Err(OrbitError::CapabilityDenied(_))
            ),
            "{}",
            tool.schema().name
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
    super::super::pipeline::invoke::OrbitPipelineInvokeTool
        .execute(&ctx, json!({"job_name":"child","input":{}}))
        .unwrap();
    assert_eq!(host.0.load(Ordering::SeqCst), 1);
    assert!(matches!(
        super::super::pipeline::invoke::OrbitPipelineInvokeTool.execute(
            &ctx,
            json!({"workspace":"ws_fixture","job_name":"child","default_input":true})
        ),
        Err(OrbitError::CapabilityDenied(_))
    ));
    assert_eq!(host.0.load(Ordering::SeqCst), 1);
}
