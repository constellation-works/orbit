//! [ORB-10971] Child-dispatch bookkeeping on a run's pipeline state.

use chrono::Utc;

use crate::workflow::{ChildCancellation, ChildCancellationPolicy, ChildDispatch, PipelineState};

fn state() -> PipelineState {
    PipelineState::new(
        "jrun-parent".to_string(),
        "workspace_auto_pipeline".to_string(),
        serde_json::json!({}),
    )
}

fn dispatch(child_run_id: &str, blocking: bool) -> ChildDispatch {
    ChildDispatch::submitted(
        child_run_id.to_string(),
        "task_auto_pipeline".to_string(),
        "invoke_and_wait".to_string(),
        blocking,
        false,
        Utc::now(),
    )
}

fn cancelled() -> ChildCancellation {
    ChildCancellation {
        policy: ChildCancellationPolicy::Cascade,
        outcome: "cancelled".to_string(),
        error: None,
        at: Utc::now(),
    }
}

/// A parent cancelled mid-dispatch: the child was linked, cancellation closed
/// the link, and the parent's own step has not yet written its late
/// checkpoints.
fn cancelled_mid_dispatch() -> (PipelineState, ChildDispatch) {
    let mut state = state();
    let mut first = dispatch("jrun-child", true);
    first.child_status = Some("running".to_string());
    first.error = Some("slow start".to_string());
    state.record_child_dispatch(first);
    assert!(state.terminalize_child_dispatch("jrun-child", cancelled()));
    let closed = state.child_dispatches[0].clone();
    (state, closed)
}

#[test]
fn a_late_re_record_cannot_reopen_a_cancelled_child() {
    let (mut state, closed) = cancelled_mid_dispatch();

    let mut late = dispatch("jrun-child", true);
    late.queued = true;
    late.submitted_at = Utc::now();
    state.record_child_dispatch(late);

    assert_eq!(state.open_child_dispatches().count(), 0);
    assert_eq!(state.child_dispatches.len(), 1);
    assert_eq!(
        state.child_dispatches[0], closed,
        "phase, status, error, cancellation, and submitted_at must all survive"
    );
}
