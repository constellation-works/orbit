//! Claimed recovery files its friction before admitting a validation retry.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use orbit_common::{Env, OrbitError};
use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
use orbit_engine::test_support::impl_test_host;
use orbit_engine::{
    DispatchError, PluginBrokerHandle, StepRecoveryDecisionRead, StepRecoveryDecisionRequest,
    StepRecoveryDecisionSlot, V2AuditWriter, execute_job_with_resume,
};
use orbit_types::workflow::activity_job::{ActivityV2Spec, DeterministicSpec, V2AuditEventKind};
use serde_json::{Value, json};

use crate::adapter::command::dispatch::claimed::owner::{
    ClaimedOwnerCall, ClaimedOwnerRoute, bridge_claimed_owner_call,
};
use crate::adapter::command::dispatch::claimed::tests::review::{FOLLOWER, Fixture, TASK};
use crate::adapter::command::dispatch::execute::ToolEntryPoint;

struct RecoveryHost<'a> {
    fixture: &'a Fixture,
    socket: &'a std::path::Path,
    attempts: AtomicUsize,
    filed: AtomicBool,
}

impl_test_host! {
    impl [<'a>] Dispatch for RecoveryHost<'a> {
        fn run_deterministic(&self, action: &str, _: &Value, input: &Value,
            _: orbit_tools::ToolContext) -> Result<Value, DispatchError> {
            if action == "validate" {
                return if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err(DispatchError::DeterministicActionFailed {
                        action: action.into(), message: "validation failed".into(),
                    })
                } else { Ok(json!({})) };
            }
            assert_eq!(action, "step_failure_recovery");
            assert_eq!(input["task_id"], TASK);
            let parent = Env::from_vars([("ORBIT_PLUGIN_BROKER", self.socket.as_os_str())]);
            let child = Env::from_vars(orbit_common::security::child_env::allowlisted_child_env(
                &parent, &[], &[],
            ));
            let request = json!({"action": "add", "body": "validation failed",
                                 "during_task": TASK});
            let call = ClaimedOwnerCall {
                binding: Some(&self.fixture.binding), process_machine_id: Some(FOLLOWER),
                name: "orbit.friction", input: &request, cwd: &self.fixture.worktree,
                workspace_root: &self.fixture.worktree, entry_point: ToolEntryPoint::Cli,
            };
            let route = bridge_claimed_owner_call(&child, &self.fixture.global_root, call);
            match route {
                Some(ClaimedOwnerRoute::Forwarded(Ok(_))) => {
                    self.filed.store(true, Ordering::SeqCst);
                    let mut decision = input["recovery_decision"].clone();
                    let path = decision["path"].as_str().unwrap().to_owned();
                    decision.as_object_mut().unwrap().remove("path");
                    decision["decision"] = json!("retry");
                    std::fs::write(path, serde_json::to_vec(&decision).unwrap()).unwrap();
                    Ok(json!({}))
                }
                other => Err(DispatchError::DeterministicActionFailed {
                    action: action.into(), message: format!("friction failed: {other:?}"),
                }),
            }
        }
    }
}

impl_test_host! { impl [<'a>] HostEnvironment for RecoveryHost<'a> {} }

impl_test_host! { impl [<'a>] RuntimeHostRunSettings for RecoveryHost<'a> {} }

impl_test_host! { impl [<'a>] RuntimeHostProcessEnvironment for RecoveryHost<'a> {} }

impl_test_host! { impl [<'a>] RunLifecycle for RecoveryHost<'a> {
    fn worker_invocation(&self) -> Option<orbit_types::tool::WorkerInvocation> {
        Some(self.fixture.binding.clone())
    }
} }

impl_test_host! { impl [<'a>] Recovery for RecoveryHost<'a> {
    fn final_recovery_log_tail(&self, _: &str) -> Result<Option<String>, OrbitError> { Ok(None) }
} }

impl_test_host! { impl [<'a>] ReviewGate for RecoveryHost<'a> {
    fn allocate_step_recovery_decision(&self, request: &StepRecoveryDecisionRequest)
        -> Result<Option<StepRecoveryDecisionSlot>, OrbitError> {
        crate::runtime::recovery_decision::allocate(request).map(Some)
    }
    fn read_step_recovery_decision(&self, slot: &StepRecoveryDecisionSlot)
        -> Result<StepRecoveryDecisionRead, OrbitError> {
        assert!(self.filed.load(Ordering::SeqCst), "friction precedes retry");
        crate::runtime::recovery_decision::read(slot)
    }
} }

#[test]
fn claimed_recovery_files_friction_then_admits_retry() {
    let fixture = Fixture::new();
    super::broker::mask(&fixture);
    let broker = fixture.serve("step_failure_recovery");
    let host = RecoveryHost {
        fixture: &fixture,
        socket: broker.socket_path(),
        attempts: AtomicUsize::new(0),
        filed: AtomicBool::new(false),
    };
    let asset = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/activities/step_failure_recovery.yaml"),
    )
    .unwrap();
    let mut recovery = load_activity_asset(&asset).unwrap().spec;
    recovery.spec = ActivityV2Spec::Deterministic(DeterministicSpec {
        action: "step_failure_recovery".into(),
        config: json!({}),
    });
    let mut job = load_job_asset(
        &json!({
            "schemaVersion": 2, "kind": "Job", "metadata": {"name": "claimed_recovery_fixture"},
            "spec": {"state": "enabled", "kind": "workflow", "steps": [{
                "id": "validate", "recovery_activity": "step_failure_recovery",
                "spec": {"type": "deterministic", "action": "validate", "config": {}},
                "default_input": {"task_id": TASK, "workspace_path": fixture.worktree,
                                  "repo_root": fixture.worktree}
            }]}
        })
        .to_string(),
    )
    .unwrap()
    .spec;
    job.steps[0].resolved_recovery_activity = Some(recovery);
    let store = orbit_store::Store::open_in_memory().unwrap();
    let writer = V2AuditWriter::with_disk_sinks(
        &fixture.worktree,
        store,
        "fixture",
        "run",
        "test-agent",
        None,
    )
    .unwrap();
    execute_job_with_resume(&job, json!({}), "run", writer.clone(), &host, None).unwrap();
    assert!(host.filed.load(Ordering::SeqCst));
    assert_eq!(host.attempts.load(Ordering::SeqCst), 2);
    assert!(
        writer
            .events_snapshot()
            .unwrap()
            .iter()
            .any(|event| matches!(event.kind, V2AuditEventKind::StepPostRecoveryAttempt { .. }))
    );
}
