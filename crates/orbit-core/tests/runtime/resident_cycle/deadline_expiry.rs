//! A mandate's deadline ends it without any resident cycle: the sweep
//! clock's owner-side expiry writes the deadline stop and hands the
//! assignment to review even while the cycle routine is disabled, and a
//! disabled cycle routine surfaces the still-active mandate in attention
//! with an enable-or-stop next action.

use super::*;

fn mandate_with_deadline(fx: &Fixture, watched: &str, deadline: Option<DateTime<Utc>>) -> String {
    let id = fx.add_task(TaskAddParams {
        title: "[orchestration]: deadline fixture - sol".into(),
        tags: vec!["resident".into()],
        ..Default::default()
    });
    let mut body = json!({
        "schema": "resident.mandate/v1",
        "goal": "Deliver the remaining scribe v2 slices",
        "exit_criteria": ["Every scribe-v2 task is done or rejected"],
        "workspaces": [fx.runtime.workspace_id().unwrap()],
        "watch": {"tasks": [watched], "tags": ["scribe-v2"]},
        "crew": "sol",
        "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120}
    });
    if let Some(deadline) = deadline {
        body["deadline"] = json!(deadline);
    }
    fx.update_task(
        &id,
        TaskUpdateParams {
            upsert_artifacts: vec![TaskArtifact::from_text(
                "resident/mandate.json",
                body.to_string(),
            )],
            ..Default::default()
        },
    );
    id
}

/// The workspace's resident cycle routine, present but disabled: no cycle
/// will ever fire until it is enabled.
fn install_disabled_cycle_routine(fx: &Fixture) {
    // The routine's target job must be a known job, or the loader retires
    // the routine instead of reporting it disabled.
    let jobs = fx.global.join("resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::write(
        jobs.join("resident_cycle_pipeline.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: resident_cycle_pipeline\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: nap\n      default_input:\n        seconds: 0\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n",
    )
    .unwrap();
    let routines = fx.repo.join(".orbit/routines");
    std::fs::create_dir_all(&routines).unwrap();
    std::fs::write(
        routines.join("resident_cycle.yaml"),
        "schemaVersion: 1\nname: resident-cycle-fixture\nenabled: false\ntrigger:\n  state:\n    kind: resident_due\n    owner_machine: fixture-machine\n    branch: main\n    debounce_minutes: 1\n    max_wait_minutes: 5\n    max_items: 50\n    batch_size: 1\n    retries: 2\n    deadline_minutes: 60\ntarget: job:resident_cycle_pipeline\n",
    )
    .unwrap();
}

fn shown(fx: &Fixture, mandate: &str) -> Value {
    call(
        &fx.automation_runtime(),
        "orbit.resident.show",
        json!({"task_id": mandate}),
        McpCapability::Operator,
    )
    .unwrap()
}

#[test]
fn a_passed_deadline_ends_the_mandate_without_any_cycle_when_the_routine_is_disabled() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::deadline_expiry::a_passed_deadline_ends_the_mandate_without_any_cycle_when_the_routine_is_disabled",
    ) {
        return;
    }
    let fx = fixture();
    install_disabled_cycle_routine(&fx);
    let watched = fx.task("Watched work");
    let now = Utc::now();
    let mandate = mandate_with_deadline(&fx, &watched, Some(now - Duration::days(2)));
    let ahead = mandate_with_deadline(&fx, &watched, Some(now + Duration::days(2)));

    let before = shown(&fx, &mandate);
    assert_eq!(before["state"], "active", "{before}");
    assert_eq!(before["cycle_routine_disabled"], true, "{before}");
    assert_eq!(
        before["cycle_routine_name"], "resident-cycle-fixture",
        "{before}"
    );

    // The sweep clock's tick on the owner: no resident cycle runs.
    let expired = fx
        .runtime
        .expire_resident_mandates_past_deadline(now)
        .unwrap();
    assert_eq!(expired, vec![mandate.clone()]);

    let after = shown(&fx, &mandate);
    assert_eq!(after["state"], "ended", "{after}");
    assert!(
        after["state_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("deadline")),
        "{after}"
    );
    assert_eq!(
        fx.runtime.get_task(&mandate).unwrap().status,
        TaskStatus::Review
    );
    let history = fx.runtime.get_task_history(&mandate).unwrap();
    assert!(
        history.iter().any(|entry| {
            entry.event == "resident_control"
                && entry
                    .note
                    .as_deref()
                    .is_some_and(|note| note.contains("\"deadline:"))
        }),
        "the deadline stop is recorded in history: {history:?}"
    );

    // A mandate whose deadline is still ahead is untouched, and expiring
    // again changes nothing.
    assert_ne!(
        fx.runtime.get_task(&ahead).unwrap().status,
        TaskStatus::Review
    );
    assert_eq!(shown(&fx, &ahead)["state"], "active");
    assert!(
        fx.runtime
            .expire_resident_mandates_past_deadline(Utc::now())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_disabled_cycle_routine_surfaces_in_attention_with_an_enable_or_stop_next_action() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::deadline_expiry::a_disabled_cycle_routine_surfaces_in_attention_with_an_enable_or_stop_next_action",
    ) {
        return;
    }
    let fx = fixture();
    install_disabled_cycle_routine(&fx);
    let watched = fx.task("Watched work");
    // Fresh mandate, two-hour heartbeat: not heartbeat-stalled, but no
    // cycle can ever fire for it while the routine is disabled.
    let mandate = mandate_with_deadline(&fx, &watched, None);
    assert_eq!(shown(&fx, &mandate)["cycle_routine_disabled"], true);

    let response = call(
        &fx.automation_runtime(),
        "orbit.attention",
        json!({"kind": "resident_stalled"}),
        McpCapability::Operator,
    )
    .unwrap();
    let item = response["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["refs"]["task_id"] == mandate)
        .unwrap_or_else(|| panic!("the mandate surfaces in attention: {response}"));
    assert_eq!(item["kind"], "resident_stalled", "{item}");
    assert_eq!(item["detail"]["cycle_routine_disabled"], true, "{item}");
    assert_eq!(item["next"]["tool"], "orbit.routine.control", "{item}");
    let summary = item["next"]["summary"].as_str().unwrap();
    assert!(
        summary.contains("Enable") && summary.contains("stop the mandate"),
        "{item}"
    );
    assert!(
        item["next"]["also"].as_array().unwrap().iter().any(|next| {
            next["tool"] == "orbit.resident.control" && next["input"]["action"] == "stop"
        }),
        "stopping the mandate is offered as a next action: {item}"
    );
}
