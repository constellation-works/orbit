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
