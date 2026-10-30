//! Independent review scheduling and recovery through the real runtime and CLI.

use chrono::Utc;
use orbit_core::application::automation::{consumer_key, evaluate_auto_task, inspect_auto_task};
use orbit_engine::RuntimeHost;
use orbit_types::workflow::automation::{BatchState, DirectLandingRequest};
use serde_json::json;

use crate::{auto_task_lifecycle_cli as lifecycle, isolated_cli_fixture::Fixture};

#[test]
fn later_review_tasks_and_inspection_do_not_wait_for_an_older_task() {
    if crate::isolated_test::run_in_child(
        "independent_reviews::later_review_tasks_and_inspection_do_not_wait_for_an_older_task",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let trigger = json!({"branch":"fixture-delivery", "threshold":1, "max_wait_minutes":60,
        "coverage":"landed_code_review_v1", "max_items":5, "retries":0});
    let (runtime, definition) = lifecycle::baselined_delivery_consumer(&fixture, &trigger);
    let consumer = consumer_key(&runtime, "auto-task", &definition.name).unwrap();
    let store = runtime.automation_store().unwrap();
    let task_id = fixture.json(&["task", "list", "--json"])[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let land = |n: usize| {
        let before = lifecycle::git(&fixture, &["rev-parse", "HEAD"]);
        std::fs::write(fixture.repo.join("fixture.txt"), format!("landing {n}\n")).unwrap();
        lifecycle::git(
            &fixture,
            &["commit", "-am", &format!("Independent landing {n}")],
        );
        let after = lifecycle::git(&fixture, &["rev-parse", "HEAD"]);
        runtime
            .record_direct_landing_intent(&DirectLandingRequest {
                run_id: format!("isolated-review-landing-{n}"),
                branch: "fixture-delivery".into(),
                before_commit: before,
                after_commit: after,
                task_ids: vec![task_id.clone()],
                handoff_id: None,
            })
            .unwrap();
    };
    land(1);
    let first = evaluate_auto_task(&runtime, &definition, false, Utc::now()).unwrap();
    assert_eq!(first.reason, "threshold_reached", "{first:?}");
    let a = first.state.unwrap().active.unwrap();
    land(2);
    let second = evaluate_auto_task(&runtime, &definition, false, Utc::now()).unwrap();
    assert_eq!(second.reason, "threshold_reached");
    let state = second.state.unwrap();
    assert_eq!(state.active.as_ref().unwrap().batch, a.batch);
    let b = state.reservations[0].clone();
    orbit_core::application::automation::release_unreferenced_attempt_pins(&runtime).unwrap();
    for attempt in [&a, &b] {
        let task = runtime
            .get_task(attempt.action_id.as_ref().unwrap())
            .unwrap();
        assert!(
            task.dependencies().is_empty(),
            "independent review tasks must not acquire task dependencies"
        );
        assert!(!task.status.is_terminal());
        let prefix = format!(
            "refs/orbit/automation/{}/{}",
            orbit_common::security::release::sha256_hex(consumer.as_bytes()),
            attempt.batch.id
        );
        assert_eq!(
            lifecycle::git(&fixture, &["rev-parse", &format!("{prefix}/through")]),
            attempt.batch.through_inclusive.commit
        );
    }
    // Inspection must count only unreserved work, not A and B again. Its
    // public path was marked no_test after earlier inspection regressions.
    assert_eq!(
        inspect_auto_task(&runtime, &definition, Utc::now())
            .unwrap()
            .reason,
        "batch_pending"
    );
    let gaps = inspect_auto_task(&runtime, &definition, Utc::now())
        .unwrap()
        .coverage_gaps;
    assert_eq!(gaps, vec![a.batch.id.clone(), b.batch.id.clone()]);
    let shown = fixture.json(&["auto-task", "show", &definition.name, "--json"]);
    assert_eq!(
        shown["automation"]["state"]["reservations"][0]["action_id"],
        b.action_id.as_deref().unwrap()
    );
    let mut exhausted = state.clone();
    exhausted.generation += 1;
    exhausted.active.as_mut().unwrap().state = BatchState::Exhausted;
    exhausted.active.as_mut().unwrap().reason = Some("isolated exhausted examination".into());
    assert!(store.automation_commit(&state, &exhausted, None).unwrap());
    land(3);
    let third = evaluate_auto_task(&runtime, &definition, false, Utc::now())
        .unwrap()
        .state
        .unwrap();
    assert_eq!(
        third.reservations.len(),
        2,
        "an exhausted review must not hold future tasks"
    );
    assert_eq!(third.reservations[0], b);
    let before_recovery = third.clone();
    let original_task = runtime.get_task(a.action_id.as_ref().unwrap()).unwrap();
    let recovered = fixture.json(&[
        "auto-task",
        "recover",
        &definition.name,
        "--batch",
        &a.batch.id,
        "--reissue-action",
        "--reason",
        "Re-examine this isolated unpaid batch",
        "--json",
    ]);
    assert_eq!(recovered["action"]["batch_id"], a.batch.id);
    assert_eq!(recovered["actions"].as_array().unwrap().len(), 3);
    let after_recovery = store.automation_state(&consumer).unwrap().unwrap();
    assert_eq!(after_recovery.reservations, before_recovery.reservations);
    assert_eq!(after_recovery.active.as_ref().unwrap().batch, a.batch);
    assert_eq!(after_recovery.active.as_ref().unwrap().attempt, 2);
    // A reset without force must notice B/C's executing actions even though
    // the oldest reservation is now an unminted replacement.
    let reset = lifecycle::operator_json(
        &fixture,
        &["auto-task", "reset", &definition.name, "--json"],
    );
    assert!(
        reset["refusals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "action_executing")
    );
    let unknown = fixture
        .command(&[
            "auto-task",
            "recover",
            &definition.name,
            "--batch",
            "unknown-batch",
            "--json",
        ])
        .assert()
        .failure();
    assert!(String::from_utf8_lossy(&unknown.get_output().stderr).contains("unknown_batch"));
    assert_eq!(
        store.automation_state(&consumer).unwrap().unwrap(),
        after_recovery
    );
    fixture
        .command(&["task", "archive", b.action_id.as_ref().unwrap()])
        .assert()
        .success();
    let wedged =
        orbit_core::application::automation::wedged_delivery_consumers(&runtime, Utc::now())
            .unwrap();
    assert!(
        wedged.iter().any(|item| item.batch_id == b.batch.id
            && item.action_id == *b.action_id.as_ref().unwrap()
            && item.terminal_status == "archived"),
        "inspection must find a later stopped action while the oldest reservation is unresolved"
    );
    assert_eq!(
        store.automation_state(&consumer).unwrap().unwrap(),
        after_recovery,
        "batch inspection is read-only"
    );
    // Manual instances keep their ordinary definition dedupe; automatic batch
    // identities alone are independent.
    let manual = fixture.json(&["auto-task", "mint", &definition.name, "--json"]);
    land(4);
    let held = evaluate_auto_task(&runtime, &definition, false, Utc::now()).unwrap();
    assert_eq!(held.reason, "open_instance");
    let held_state = held.state.unwrap();
    assert_eq!(held_state.reservations.len(), 2);
    assert_eq!(
        held_state.observed.commit,
        lifecycle::git(&fixture, &["rev-parse", "HEAD"])
    );
    let replacement = runtime
        .get_task(
            held_state
                .active
                .as_ref()
                .unwrap()
                .action_id
                .as_ref()
                .unwrap(),
        )
        .unwrap();
    assert!(
        replacement
            .description
            .contains(a.action_id.as_ref().unwrap()),
        "a replacement keeps the original artifact owner discoverable"
    );
    assert_eq!(
        runtime.get_task(a.action_id.as_ref().unwrap()).unwrap(),
        original_task
    );
    fixture
        .command(&["task", "archive", manual["id"].as_str().unwrap()])
        .assert()
        .success();
    let resumed = evaluate_auto_task(&runtime, &definition, false, Utc::now())
        .unwrap()
        .state
        .unwrap();
    assert_eq!(resumed.reservations.len(), 3);
}
