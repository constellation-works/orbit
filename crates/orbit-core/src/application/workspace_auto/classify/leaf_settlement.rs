//! Record a terminal `task_auto_pipeline` child on its parent dispatch.
//!
//! Detached invocation checkpoints the child as submitted and returns. The
//! drain's admission pass is the recurring observer: once that child run is
//! terminal, the dispatch phase becomes terminal and `child_status` is the
//! run's final status. A missing child, a live child, and any other job stay
//! as they are. The approval pilot has its own record and is not settled here.
//! A store failure is logged and the pass continues; the next pass retries.

use orbit_types::workflow::ChildDispatchPhase;

use crate::OrbitRuntime;
use crate::application::workspace_auto::drains::{LEAF_JOB_NAME, drain_run_id};

use serde_json::Value;

/// Advance each open leaf dispatch whose child run has already ended.
pub(super) fn settle_terminal_leaf_dispatches(runtime: &OrbitRuntime, input: &Value) {
    let Some(run_id) = drain_run_id(input) else {
        return;
    };
    let state = match runtime.read_run_state(run_id) {
        Ok(Some(state)) => state,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(
                target: "orbit.core.job_run",
                run_id,
                %error,
                "drain could not read its state to record finished leaves"
            );
            return;
        }
    };
    let jobs = runtime.stores().jobs();
    let mut terminal = Vec::new();
    for dispatch in state.open_child_dispatches() {
        if dispatch.job_name != LEAF_JOB_NAME {
            continue;
        }
        match jobs.get_job_run(&dispatch.child_run_id) {
            Ok(Some(run)) if run.state.is_terminal() => {
                terminal.push((dispatch.child_run_id.clone(), run.state.to_string()));
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    target: "orbit.core.job_run",
                    run_id,
                    child_run_id = %dispatch.child_run_id,
                    %error,
                    "drain could not read a leaf to record its dispatch"
                );
            }
        }
    }
    if terminal.is_empty() {
        return;
    }
    let result = jobs.update_run_state(run_id, &mut |_, state| {
        for (child_run_id, status) in &terminal {
            state.advance_child_dispatch(
                child_run_id,
                ChildDispatchPhase::Terminal,
                Some(status.clone()),
                None,
            );
        }
        Ok(())
    });
    if let Err(error) = result {
        tracing::warn!(
            target: "orbit.core.job_run",
            run_id,
            %error,
            "drain could not record finished leaves; a later pass will retry"
        );
    }
}
