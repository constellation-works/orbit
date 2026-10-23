//! A system writer must never borrow a human actor for task history.

use super::support::gated_fixture;

/// Safety seam: settlement currently emits no history stubs, so its public
/// boundary cannot provoke a history-producing system edit. Guard the
/// shared deterministic writer against false human intervention [ORB-13916].
#[test]
fn system_writes_cannot_borrow_the_operator_for_history() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &system_writes_cannot_borrow_the_operator_for_history,
    )) {
        return;
    }
    let mut gated = gated_fixture("");
    gated.fixture.runtime = gated
        .fixture
        .runtime
        .with_actor(crate::ActorIdentity::human("human:daniel"));
    let runtime = &gated.fixture.runtime;
    let before = runtime.get_task_history(&gated.task_id).expect("history");
    runtime
        .update_task_as_system(
            &gated.task_id,
            crate::application::task::TaskUpdateParams {
                title: Some("System-updated fixture".to_string()),
                ..Default::default()
            },
            Some(gated.run_id),
        )
        .expect("system edit");
    let history = runtime.get_task_history(&gated.task_id).expect("history");
    assert_eq!(&history[..before.len()], before.as_slice());
    assert_eq!(history.len(), before.len() + 1);
    assert_eq!(history[before.len()].event, "renamed");
    assert_eq!(
        history[before.len()].by,
        "system",
        "ORB-13916: deterministic history must not masquerade as human intervention"
    );
}
