//! [ORB-10971] Child-dispatch bookkeeping on a run's pipeline state.

use chrono::Utc;

use crate::workflow::{
    ChildCancellation, ChildCancellationPolicy, ChildDispatch, ChildDispatchPhase, PipelineState,
};

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

#[test]
fn re_recording_the_same_child_updates_it_rather_than_duplicating_it() {
    // A resumed or retried parent re-executes its dispatch step. The child is
    // one child, not two rows.
    let mut state = state();
    let first = dispatch("jrun-child", true);
    let submitted_at = first.submitted_at;
    state.record_child_dispatch(first);

    let mut second = dispatch("jrun-child", true);
    second.queued = true;
    second.submitted_at = Utc::now();
    state.record_child_dispatch(second);

    assert_eq!(state.child_dispatches.len(), 1);
    assert!(state.child_dispatches[0].queued);
    assert_eq!(
        state.child_dispatches[0].submitted_at, submitted_at,
        "the observable submission instant must not drift on re-record"
    );
}

#[test]
fn advancing_an_unrecorded_child_reports_the_lost_checkpoint() {
    let mut state = state();
    assert!(
        !state.advance_child_dispatch("jrun-missing", ChildDispatchPhase::Waiting, None, None),
        "a caller must be able to tell a lost checkpoint from a successful update"
    );
}

#[test]
fn clearing_waiting_reasons_leaves_child_lineage_intact() {
    // Cancellation clears the waiting reasons. The child link must survive it:
    // it is the only handle on the work the parent left behind.
    let mut state = state();
    state.set_waiting_reasons(Some(vec!["ORB-1".to_string()]), None);
    state.record_child_dispatch(dispatch("jrun-child", true));

    state.clear_waiting_reasons();

    assert_eq!(state.waiting_on_deps, None);
    assert_eq!(state.child_dispatches.len(), 1);
}

#[test]
fn only_blocking_children_are_cascade_targets() {
    let mut state = state();
    state.record_child_dispatch(dispatch("jrun-blocking", true));
    state.record_child_dispatch(dispatch("jrun-detached", false));

    assert_eq!(state.cascade_cancellation_targets(), vec!["jrun-blocking"]);
}

#[test]
fn a_terminalized_child_is_no_longer_open_but_stays_recorded() {
    let mut state = state();
    state.record_child_dispatch(dispatch("jrun-child", true));

    assert!(state.terminalize_child_dispatch(
        "jrun-child",
        ChildCancellation {
            policy: ChildCancellationPolicy::Cascade,
            outcome: "cancelled".to_string(),
            error: None,
            at: Utc::now(),
        },
    ));

    assert_eq!(state.open_child_dispatches().count(), 0);
    assert!(state.cascade_cancellation_targets().is_empty());
    assert_eq!(state.child_dispatches.len(), 1);
    assert_eq!(
        state.child_dispatches[0].phase,
        ChildDispatchPhase::Terminal
    );
}

#[test]
fn a_state_without_children_round_trips_without_the_field() {
    // Existing on-disk `state.json` files predate this field and must keep
    // decoding, and a childless run must not start emitting an empty array.
    let encoded = serde_json::to_value(state()).expect("encode");
    assert!(
        !encoded
            .as_object()
            .expect("object")
            .contains_key("child_dispatches")
    );

    let decoded: PipelineState = serde_json::from_value(encoded).expect("decode");
    assert!(decoded.child_dispatches.is_empty());
}

// [ORB-11253] The live worker ceiling of a bounded drain.

#[test]
fn an_absent_control_leaves_the_submitted_ceiling_in_force() {
    let state = state();
    assert_eq!(state.effective_max_active_leaf_runs(5), 5);
    assert_eq!(state.drain_worker_limit_revision(), 0);
}

#[test]
fn setting_the_ceiling_records_what_it_replaced_and_advances_the_revision() {
    let mut state = state();

    assert!(state.set_drain_worker_limit(7, 5, "operator".to_string(), None, Some(0)));

    let limit = state.drain_worker_limit.clone().expect("limit recorded");
    assert_eq!(limit.max_active_leaf_runs, 7);
    assert_eq!(limit.previous_max_active_leaf_runs, 5);
    assert_eq!(limit.revision, 1);
    assert_eq!(limit.actor, "operator");
    assert_eq!(state.effective_max_active_leaf_runs(5), 7);

    // The second change replaces the first, not the submitted value.
    assert!(state.set_drain_worker_limit(
        3,
        5,
        "operator".to_string(),
        Some("provider throttled".to_string()),
        Some(1),
    ));
    let limit = state.drain_worker_limit.clone().expect("limit recorded");
    assert_eq!(limit.max_active_leaf_runs, 3);
    assert_eq!(limit.previous_max_active_leaf_runs, 7);
    assert_eq!(limit.revision, 2);
    assert_eq!(limit.reason.as_deref(), Some("provider throttled"));
}

#[test]
fn a_stale_expected_revision_changes_nothing() {
    // Two operators read revision 0; the first wins, and the second must not
    // silently overwrite the ceiling it never saw.
    let mut state = state();
    assert!(state.set_drain_worker_limit(7, 5, "first".to_string(), None, Some(0)));

    assert!(!state.set_drain_worker_limit(2, 5, "second".to_string(), None, Some(0)));

    let limit = state.drain_worker_limit.clone().expect("limit recorded");
    assert_eq!(limit.max_active_leaf_runs, 7);
    assert_eq!(limit.revision, 1);
    assert_eq!(limit.actor, "first");
}

#[test]
fn an_unconditional_write_replaces_whatever_is_recorded() {
    let mut state = state();
    assert!(state.set_drain_worker_limit(7, 5, "first".to_string(), None, None));
    assert!(state.set_drain_worker_limit(2, 5, "second".to_string(), None, None));
    assert_eq!(state.effective_max_active_leaf_runs(5), 2);
    assert_eq!(state.drain_worker_limit_revision(), 2);
}

#[test]
fn the_control_survives_a_state_round_trip() {
    let mut state = state();
    state.set_drain_worker_limit(7, 5, "operator".to_string(), None, None);
    let encoded = serde_json::to_string(&state).expect("serialize state");
    let decoded: PipelineState = serde_json::from_str(&encoded).expect("deserialize state");
    assert_eq!(decoded.drain_worker_limit, state.drain_worker_limit);
}

#[test]
fn state_written_before_the_control_existed_still_loads() {
    let legacy = serde_json::json!({
        "run_id": "jrun-legacy",
        "job_id": "workspace_auto_pipeline",
        "initial_input": {},
        "pipeline": {},
        "updated_at": Utc::now().to_rfc3339(),
    });
    let decoded: PipelineState = serde_json::from_value(legacy).expect("deserialize legacy state");
    assert!(decoded.drain_worker_limit.is_none());
    assert!(decoded.drain_admissions_stop.is_none());
    assert!(!decoded.admissions_stopped());
    assert_eq!(decoded.effective_max_active_leaf_runs(5), 5);
}

// [ORB-11283] Stop new admissions without cancelling the coordinator.

#[test]
fn an_absent_stop_means_the_drain_is_still_admitting() {
    let state = state();
    assert!(!state.admissions_stopped());
    assert!(state.drain_admissions_stop.is_none());
}

#[test]
fn setting_the_stop_records_the_actor_and_is_idempotent() {
    let mut state = state();
    assert!(state.set_drain_admissions_stop(
        "operator".to_string(),
        Some("window closed early".to_string()),
    ));
    let first = state.drain_admissions_stop.clone().expect("stop recorded");
    assert_eq!(first.actor, "operator");
    assert_eq!(first.reason.as_deref(), Some("window closed early"));
    assert!(state.admissions_stopped());

    assert!(!state.set_drain_admissions_stop("second".to_string(), None));
    let second = state
        .drain_admissions_stop
        .clone()
        .expect("original stop kept");
    assert_eq!(second.actor, "operator");
    assert_eq!(second.stopped_at, first.stopped_at);
}

#[test]
fn the_stop_survives_a_state_round_trip_and_is_omitted_when_absent() {
    let encoded = serde_json::to_value(state()).expect("encode");
    assert!(
        !encoded
            .as_object()
            .expect("object")
            .contains_key("drain_admissions_stop")
    );

    let mut state = state();
    state.set_drain_admissions_stop("cli".to_string(), None);
    let encoded = serde_json::to_string(&state).expect("serialize state");
    let decoded: PipelineState = serde_json::from_str(&encoded).expect("deserialize state");
    assert_eq!(decoded.drain_admissions_stop, state.drain_admissions_stop);
}
