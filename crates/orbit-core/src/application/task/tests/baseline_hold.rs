//! One unreadable task history must not abort the baseline-hold tick.

use std::time::{Duration, Instant};

use orbit_engine::{RuntimeHost, baseline_red_hold_update};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{
    BASELINE_RED_HOLD_VERDICT_EVENT, BaselineRedFailures, BaselineRedHold,
};

use super::{enter_isolated_child, test_runtime};
use crate::application::task::TaskAddParams;
use crate::application::task::baseline_hold::FAIL_BASELINE_HOLD_HISTORY;

fn hold(run_id: &str) -> BaselineRedHold {
    BaselineRedHold {
        base_ref: String::new(),
        base_sha: "a".repeat(40),
        command: "true".into(),
        run_id: run_id.into(),
        selection: None,
        failures: BaselineRedFailures::default(),
    }
}

#[test]
fn one_unreadable_history_does_not_abort_the_baseline_hold_tick() {
    if !enter_isolated_child(
        module_path!(),
        "one_unreadable_history_does_not_abort_the_baseline_hold_tick",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let mut held = Vec::new();
    for index in 0..2 {
        let task = runtime
            .add_task(TaskAddParams {
                title: format!("Held task {index}"),
                description: "A task under a baseline hold.".into(),
                acceptance_criteria: vec!["The hold is judged.".into()],
                ..Default::default()
            })
            .expect("add task");
        RuntimeHost::apply_task_automation_update(
            &runtime,
            &task.id,
            baseline_red_hold_update("task_pr_pipeline", &hold(&format!("jrun-hold-{index}"))),
        )
        .expect("record the hold");
        assert_eq!(
            runtime.get_task(&task.id).expect("task").status,
            TaskStatus::Backlog
        );
        held.push(task.id);
        std::thread::sleep(Duration::from_millis(5));
    }

    let listed = runtime
        .list_tasks_filtered(Some(TaskStatus::Backlog), None, None, None, None, None)
        .expect("backlog");
    assert_eq!(listed.len(), 2, "{listed:?}");
    let unreadable = listed[0].id.clone();
    let readable = listed[1].id.clone();
    assert!(held.contains(&unreadable) && held.contains(&readable));

    FAIL_BASELINE_HOLD_HISTORY.with(|fail| *fail.borrow_mut() = Some(unreadable.clone()));
    let refresh = runtime
        .run_baseline_hold_tick(Instant::now() + Duration::from_secs(60))
        .expect("one unreadable history does not fail the tick");
    assert!(
        refresh.held.contains(&readable),
        "the later hold is still judged: {refresh:?}"
    );
    assert!(
        !refresh.held.contains(&unreadable),
        "the unreadable task is skipped, not failed: {refresh:?}"
    );

    let judged = runtime
        .get_task_history(&readable)
        .expect("readable history");
    assert!(
        judged
            .iter()
            .any(|entry| entry.event == BASELINE_RED_HOLD_VERDICT_EVENT),
        "the readable hold records a verdict: {judged:?}"
    );
    let skipped = runtime
        .get_task_history(&unreadable)
        .expect("the history is readable outside the seam");
    assert!(
        skipped
            .iter()
            .all(|entry| entry.event != BASELINE_RED_HOLD_VERDICT_EVENT),
        "the unreadable task gets no verdict from the aborted read: {skipped:?}"
    );
}
