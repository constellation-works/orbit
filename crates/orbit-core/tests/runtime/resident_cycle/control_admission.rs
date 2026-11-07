//! Manager control wins over a cycle that has only observed its mandate.

use orbit_core::OrbitError;
use orbit_core::test_support::with_resident_cycle_admission_checkpoint;
use orbit_types::workflow::RESIDENT_CYCLE_GRANTED_EVENT;

use super::*;

#[test]
fn control_fences_admission_but_allows_an_admitted_cycle_to_finish() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::control_admission::control_fences_admission_but_allows_an_admitted_cycle_to_finish",
    ) {
        return;
    }
    for action in ["pause", "takeover", "stop"] {
        let fx = fixture();
        let watched = fx.task("Watched work");
        let assignment = || {
            let mandate = fx.mandate(&watched);
            fx.runtime
                .update_task_as_human(
                    &mandate,
                    TaskUpdateParams {
                        plan: Some("Run cycles until stopped.".into()),
                        status: Some(TaskStatus::InProgress),
                        ..Default::default()
                    },
                    "human:daniel".into(),
                )
                .unwrap();
            fx.grant(&mandate, &["merge"]);
            mandate
        };
        let mandate = assignment();
        let run = fx.cycle_run(&mandate, "control-before-admission");
        let runtime = fx.runtime.clone();
        let controlled_mandate = mandate.clone();
        let admission = with_resident_cycle_admission_checkpoint(
            move || {
                // Observe already read Active, but has not published a live
                // cycle. Complete the competing public control call first.
                let controlled = call(
                    &runtime,
                    "orbit.resident.control",
                    json!({"task_id": controlled_mandate, "action": action,
                           "reason": "Control wins before publication"}),
                    McpCapability::Operator,
                )
                .unwrap();
                if action == "stop" {
                    assert_eq!(controlled["handoff"], "review");
                }
            },
            || fx.runtime.begin_resident_cycle(&run, Utc::now()),
        );
        assert!(
            matches!(admission, Err(OrbitError::PolicyDenied(_))),
            "{action}: a stale observe must not admit a cycle: {admission:?}"
        );
        let shown = fx.runtime.show_resident_mandate(&mandate).unwrap();
        assert_eq!(shown["control"]["last"]["action"], action, "{shown}");
        assert!(shown["live_cycle"].is_null(), "{action}: {shown}");
        assert!(
            fx.runtime
                .get_task_history(&mandate)
                .unwrap()
                .iter()
                .all(|entry| entry.event != RESIDENT_CYCLE_GRANTED_EVENT),
            "{action}: refused admission must not publish a frozen grant"
        );
        assert_eq!(
            fx.runtime.finish_resident_cycle(&run).unwrap()["finalized"],
            false
        );

        // Reverse the order: publication wins, so control must let the live
        // cycle checkpoint and finalize using its already frozen grant.
        let mandate = assignment();
        let run = fx.cycle_run(&mandate, "control-after-admission");
        let brief = fx.runtime.begin_resident_cycle(&run, Utc::now()).unwrap();
        assert_eq!(brief["cycle_count"], 1);
        let controlled = call(
            &fx.runtime,
            "orbit.resident.control",
            json!({"task_id": mandate, "action": action,
                   "reason": "An admitted cycle may finish"}),
            McpCapability::Operator,
        )
        .unwrap();
        if action == "stop" {
            assert_eq!(controlled["handoff"], "cycle_live");
        }
        fx.checkpoint(&brief, &run);
        let finished = fx.runtime.finish_resident_cycle(&run).unwrap();
        assert_eq!(finished["finalized"], true, "{action}: {finished}");
        assert_eq!(finished["synthesized"], false, "{action}: {finished}");
        if action == "stop" {
            assert_eq!(finished["handoff"], "review");
        }
        let shown = fx.runtime.show_resident_mandate(&mandate).unwrap();
        assert!(shown["live_cycle"].is_null(), "{action}: {shown}");
        let next_run = fx.cycle_run(&mandate, "control-holds-next-admission");
        let next = fx
            .runtime
            .begin_resident_cycle(&next_run, Utc::now())
            .unwrap();
        assert_eq!(next["cycle_count"], 0, "{action}: {next}");
        assert_eq!(
            next["skip_reason"], "mandate_not_active",
            "{action}: {next}"
        );
    }
}

#[test]
fn status_request_wake_between_observe_and_publication_reaches_the_admitted_cycle() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::control_admission::status_request_wake_between_observe_and_publication_reaches_the_admitted_cycle",
    ) {
        return;
    }
    let fx = fixture();
    let watched = fx.task("Watched work");
    let mandate = fx.mandate(&watched);
    fx.runtime
        .update_task_as_human(
            &mandate,
            TaskUpdateParams {
                plan: Some("Run cycles until stopped.".into()),
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();
    fx.grant(&mandate, &["merge"]);
    let run = fx.cycle_run(&mandate, "status-request-before-publication");
    let runtime = fx.runtime.clone();
    let controlled_mandate = mandate.clone();
    let brief = with_resident_cycle_admission_checkpoint(
        move || {
            call(
                &runtime,
                "orbit.resident.control",
                json!({"task_id": controlled_mandate, "action": "wake",
                       "reason": "Status please", "status_request": true}),
                McpCapability::Operator,
            )
            .unwrap();
        },
        || fx.runtime.begin_resident_cycle(&run, Utc::now()),
    )
    .unwrap();
    assert_eq!(brief["cycle_count"], 1, "{brief}");
    assert_eq!(brief["status_request"], true, "{brief}");
    let reasons: Vec<String> = brief["wake"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|reason| reason.as_str().map(str::to_string))
        .collect();
    assert!(reasons.contains(&"wake".to_string()), "{brief}");
    assert!(reasons.contains(&"status_request".to_string()), "{brief}");
    fx.checkpoint(&brief, &run);
    let finished = fx.runtime.finish_resident_cycle(&run).unwrap();
    assert_eq!(finished["finalized"], true, "{finished}");
    assert_eq!(finished["synthesized"], false, "{finished}");
}

#[test]
fn invalid_mandate_refuses_control_except_stop_without_history() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::control_admission::invalid_mandate_refuses_control_except_stop_without_history",
    ) {
        return;
    }
    let fx = fixture();
    let invalid = fx
        .runtime
        .add_task(TaskAddParams {
            title: "[orchestration]: broken - sol".into(),
            tags: vec!["resident".into()],
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let shown = fx.runtime.show_resident_mandate(&invalid).unwrap();
    assert_eq!(shown["state"], "invalid", "{shown}");

    for action in ["pause", "resume", "wake", "takeover", "release"] {
        let before = fx.runtime.get_task_history(&invalid).unwrap().len();
        let refused = call(
            &fx.runtime,
            "orbit.resident.control",
            json!({"task_id": invalid, "action": action,
                   "reason": "An invalid mandate stays put"}),
            McpCapability::Operator,
        )
        .unwrap_err();
        assert!(
            refused.contains("resident_control_refused"),
            "{action}: refusal must name the control gate: {refused}"
        );
        assert!(
            refused.contains("invalid"),
            "{action}: refusal must name the invalid mandate state: {refused}"
        );
        let after = fx.runtime.get_task_history(&invalid).unwrap().len();
        assert_eq!(
            before, after,
            "{action}: a refused control must write no history"
        );
    }

    let stopped = call(
        &fx.runtime,
        "orbit.resident.control",
        json!({"task_id": invalid, "action": "stop",
               "reason": "Dispose of the broken mandate"}),
        McpCapability::Operator,
    )
    .unwrap();
    assert_eq!(stopped["state"], "ended", "{stopped}");
    assert_eq!(stopped["handoff"], "review", "{stopped}");
}
