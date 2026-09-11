use super::super::format::*;
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::json;

fn state_with_waiting(deps: Option<Vec<&str>>, locks: Option<Vec<&str>>) -> PipelineState {
    let mut state = PipelineState::new("jrun-test".to_string(), "job-test".to_string(), json!({}));
    state.set_waiting_reasons(
        deps.map(|values| values.into_iter().map(str::to_string).collect()),
        locks.map(|values| values.into_iter().map(str::to_string).collect()),
    );
    state
}

#[test]
fn waiting_line_lists_deps_and_locks_for_waiting_run() {
    let state = state_with_waiting(Some(vec!["ORB-1", "ORB-2"]), Some(vec!["file:src/lib.rs"]));

    assert_eq!(
        format_waiting_line(JobRunState::Running, Some(&state)),
        Some("Waiting on deps: ORB-1, ORB-2; locks: file:src/lib.rs".to_string())
    );
}

#[test]
fn waiting_line_omits_non_waiting_run() {
    let state = PipelineState::new("jrun-test".to_string(), "job-test".to_string(), json!({}));

    assert_eq!(
        format_waiting_line(JobRunState::Running, Some(&state)),
        None
    );
}

#[test]
fn waiting_line_omits_terminal_run_even_with_stale_reasons() {
    let state = state_with_waiting(Some(vec!["ORB-1"]), Some(vec!["file:src/lib.rs"]));

    assert_eq!(
        format_waiting_line(JobRunState::Success, Some(&state)),
        None
    );
}

/// [ORB-10971] The child-lineage lines an operator sees on `orbit run show`.
#[test]
fn child_dispatch_lines_name_the_child_run_job_and_phase() {
    let mut state = PipelineState::new("jrun-test".to_string(), "job-test".to_string(), json!({}));
    state.record_child_dispatch(
        orbit_types::workflow::ChildDispatch::submitted(
            "jrun-child-leaves".to_string(),
            "task_auto_pipeline".to_string(),
            "invoke_and_wait".to_string(),
            true,
            false,
            chrono::Utc::now(),
        )
        .with_parent_step_id(Some("ship_leaves".to_string())),
    );
    state.advance_child_dispatch(
        "jrun-child-leaves",
        orbit_types::workflow::ChildDispatchPhase::Waiting,
        None,
        None,
    );

    assert_eq!(
        format_child_dispatch_lines(Some(&state)),
        vec![
            "Child jrun-child-leaves job=task_auto_pipeline step=ship_leaves phase=waiting queued=false"
                .to_string()
        ]
    );
}

#[test]
fn child_dispatch_lines_survive_a_cancelled_parent_and_name_the_policy() {
    let mut state = PipelineState::new("jrun-test".to_string(), "job-test".to_string(), json!({}));
    state.record_child_dispatch(orbit_types::workflow::ChildDispatch::submitted(
        "jrun-child-leaves".to_string(),
        "task_auto_pipeline".to_string(),
        "invoke_and_wait".to_string(),
        true,
        false,
        chrono::Utc::now(),
    ));
    state.terminalize_child_dispatch(
        "jrun-child-leaves",
        orbit_types::workflow::ChildCancellation {
            policy: orbit_types::workflow::ChildCancellationPolicy::Cascade,
            outcome: "cancelled".to_string(),
            error: None,
            at: chrono::Utc::now(),
        },
    );

    let lines = format_child_dispatch_lines(Some(&state));
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("phase=terminal"), "{}", lines[0]);
    assert!(
        lines[0].contains("cancellation=cascade/cancelled"),
        "{}",
        lines[0]
    );
}

#[test]
fn no_child_dispatches_prints_nothing() {
    let state = PipelineState::new("jrun-test".to_string(), "job-test".to_string(), json!({}));
    assert!(format_child_dispatch_lines(Some(&state)).is_empty());
    assert!(format_child_dispatch_lines(None).is_empty());
}

/// [ORB-11253] A retuned drain says so in `orbit run show`; an untouched one
/// stays quiet, because its submitted input already answers the question.
#[test]
fn worker_limit_line_reports_the_change_and_its_author() {
    let mut state = PipelineState::new(
        "jrun-drain".to_string(),
        "workspace_auto_pipeline".to_string(),
        serde_json::json!({ "max_active_leaf_runs": 5 }),
    );
    assert_eq!(format_worker_limit_line(Some(&state)), None);

    state.set_drain_worker_limit(
        7,
        5,
        "operator".to_string(),
        Some("more headroom".to_string()),
        None,
    );
    let line = format_worker_limit_line(Some(&state)).expect("worker limit line");
    assert!(line.contains("Workers: 7"), "{line}");
    assert!(line.contains("was 5"), "{line}");
    assert!(line.contains("revision 1"), "{line}");
    assert!(line.contains("set by operator"), "{line}");
    assert!(line.contains("reason=more headroom"), "{line}");
}

/// [ORB-11283] A stopped drain says so in `orbit run show`.
#[test]
fn admissions_stop_line_names_the_actor_and_is_absent_when_unset() {
    let mut state = PipelineState::new(
        "jrun-drain".to_string(),
        "workspace_auto_pipeline".to_string(),
        json!({}),
    );
    assert_eq!(format_admissions_stop_line(Some(&state)), None);

    state.set_drain_admissions_stop("operator".to_string(), Some("done for the day".to_string()));
    let line = format_admissions_stop_line(Some(&state)).expect("stop line");
    assert!(line.contains("Admissions: stopped by operator"), "{line}");
    assert!(line.contains("reason=done for the day"), "{line}");
}

/// [ORB-12111] The role a run id declares is what keeps a `run history`
/// listing of same-minute runs from reading as one run tree.
#[test]
fn run_role_reads_the_id_and_admits_when_it_cannot() {
    assert_eq!(format_run_role("jrun-20260911-0146-t2"), "top-level");
    assert_eq!(format_run_role("jrun-20260911-0146-c2"), "child");
    assert_eq!(format_run_role("jrun-20260911-0146-2"), "unmarked");
    assert_eq!(format_run_role("jrun-20260911-0146"), "unmarked");
}
