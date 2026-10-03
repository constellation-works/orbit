//! Execute the shipped workspace-ship wrapper with a synthetic child receipt.
use std::sync::{Arc, Mutex};

use orbit_engine::{DispatchError, JobOutcome, RuntimeHost};
use orbit_tools::{FsAuditLogger, ToolContext};
use serde_json::{Value, json};

use super::{seed_default_catalogs, test_runtime, try_execute_named_job};
use crate::OrbitRuntime;

struct ShipHost<'a> {
    runtime: &'a OrbitRuntime,
    receipt: Value,
    dispatch_error: bool,
    calls: Mutex<Vec<(String, Value)>>,
}

impl RuntimeHost for ShipHost<'_> {
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        context: ToolContext,
    ) -> Result<Value, DispatchError> {
        self.calls
            .lock()
            .unwrap()
            .push((action.to_owned(), input.clone()));
        match action {
            "invoke_and_wait" if self.dispatch_error => Err(
                DispatchError::DeterministicActionNotRegistered("fixture child unavailable".into()),
            ),
            "invoke_and_wait" => Ok(self.receipt.clone()),
            "pipeline_success_guard" => <OrbitRuntime as RuntimeHost>::run_deterministic(
                self.runtime,
                action,
                config,
                input,
                context,
            ),
            _ => Err(DispatchError::DeterministicActionNotRegistered(
                action.into(),
            )),
        }
    }
    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<Arc<dyn FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> ToolContext {
        <OrbitRuntime as RuntimeHost>::tool_context_for_activity(
            self.runtime,
            run_id,
            fs_profile,
            fs_audit,
            proc_allowed_programs,
        )
    }
}

fn execute(
    receipt: Value,
    dispatch_error: bool,
) -> (Result<JobOutcome, DispatchError>, Vec<(String, Value)>) {
    let (root, runtime, repo, global) = test_runtime();
    seed_default_catalogs(&global);
    let host = ShipHost {
        runtime: &runtime,
        receipt,
        dispatch_error,
        calls: Mutex::new(Vec::new()),
    };
    let id = root.path().file_name().unwrap().to_string_lossy();
    let result = try_execute_named_job(
        &runtime,
        &repo,
        &host,
        "workspace_ship_pipeline",
        json!({}),
        &id,
    );
    (result, host.calls.into_inner().unwrap())
}

#[test]
fn shipped_workspace_ship_waits_for_child_before_guarding_its_receipt() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &shipped_workspace_ship_waits_for_child_before_guarding_its_receipt,
    )) {
        return;
    }
    let receipt = json!({"run_id":"fixture-auto", "status":"succeeded"});
    let (result, calls) = execute(receipt.clone(), false);
    assert!(result.expect("ship wrapper").success);
    assert_eq!(
        calls.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
        ["invoke_and_wait", "pipeline_success_guard"]
    );
    assert_eq!(calls[0].1["job_name"], "workspace_auto_pipeline");
    assert_eq!(calls[0].1["run_input"]["for_seconds"], 1200);
    assert_eq!(calls[1].1["result"], receipt);
}

#[test]
fn shipped_workspace_ship_propagates_child_failure() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &shipped_workspace_ship_propagates_child_failure,
    )) {
        return;
    }
    let (result, calls) = execute(
        json!({"run_id":"fixture-auto", "status":"failed", "error":"fixture drain refused"}),
        false,
    );
    let error = result.expect_err("failed child cannot succeed").to_string();
    assert!(error.contains("fixture drain refused"), "{error}");
    assert_eq!(calls.len(), 2);
}

#[test]
fn shipped_workspace_ship_stops_before_guard_when_child_dispatch_fails() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &shipped_workspace_ship_stops_before_guard_when_child_dispatch_fails,
    )) {
        return;
    }
    let (result, calls) = execute(Value::Null, true);
    let error = result.expect_err("dispatch failure").to_string();
    assert!(error.contains("fixture child unavailable"), "{error}");
    assert_eq!(calls.len(), 1);
}
