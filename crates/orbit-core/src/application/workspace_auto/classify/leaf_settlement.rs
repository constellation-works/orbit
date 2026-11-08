//! Record a terminal `task_auto_pipeline` child on its parent dispatch.
//!
//! Detached invocation checkpoints the child as submitted and returns. Two
//! observers then record a child run that has ended: the dispatch phase
//! becomes terminal and `child_status` is the run's final status.
//! - The drain's admission pass, on every iteration.
//! - A reconciling read of the drain run ([`OrbitRuntime::show_job_run`],
//!   behind `orbit run show`, HTTP and MCP). A leaf can finish after the
//!   drain's last pass: a window that expired while it ran, or a default
//!   single-tick drain whose leaves all outlive it. No pass runs after the
//!   loop, and the child run does not record its parent, so the read is
//!   the observer that settles those leaves [ORB-23198].
//!
//! A missing child, a live child, and any other job stay as they are. The
//! approval pilot has its own record and is not settled here. A store failure
//! is logged and the caller continues; the next pass or read retries.

use orbit_types::workflow::{ChildDispatchPhase, JobRun};

use crate::OrbitRuntime;
use crate::application::job::JobDrainLeafPort;
use crate::application::workspace_auto::drains::{DRAIN_JOB_NAME, LEAF_JOB_NAME};

impl JobDrainLeafPort for OrbitRuntime {
    fn record_finished_drain_leaves(&self, run: &JobRun) {
        if run.job_id == DRAIN_JOB_NAME {
            settle_terminal_leaf_dispatches(self, &run.run_id);
        }
    }
}

/// Advance each open leaf dispatch whose child run has already ended.
pub(super) fn settle_terminal_leaf_dispatches(runtime: &OrbitRuntime, run_id: &str) {
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
            "drain could not record finished leaves; a later pass or read will retry"
        );
    }
}
