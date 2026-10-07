//! A minted review task may close while its attempt still says Claimed.
//! Recovery and ticks must retain the unpaid batch across definition edits.

use chrono::{Duration, Utc};
use orbit_core::application::auto_tasks::scheduler::{
    SchedulerOptions, run_auto_task_scheduler_at,
};
use orbit_core::application::automation::{consumer_key, evaluate_auto_task};
use orbit_types::workflow::automation::{
    AcceptedCoverage, AutomationState, BatchState, ExaminationCheck, evidence_template,
};
use serde_json::{Value, json};

use crate::auto_task_lifecycle_cli::git;
use crate::isolated_cli_fixture::Fixture;
use crate::review_after_landing_cli::{
    commit, doctor_row, enable_review_crew, in_isolated_child, land_and_evaluate, open_runtime,
    retarget, toggle, trigger,
};

const CONSUMER: &str = "delivery-code-review";

/// Mint real keyed tasks, retain an accepted receipt from an earlier batch,
/// and simulate either side of the action-id checkpoint crash window.
fn claimed(checkpointed_id: bool) -> (Fixture, orbit_core::OrbitRuntime, AutomationState, String) {
    let fixture = Fixture::new();
    enable_review_crew(&fixture);
    git(&fixture, &["checkout", "-b", "fixture-delivery"]);
    commit(&fixture, "baseline\n");
    retarget(&fixture, &trigger());
    toggle(&fixture, "on");
    let runtime = open_runtime(&fixture);
    let consumer = consumer_key(&runtime, "auto-task", CONSUMER).unwrap();
    let store = runtime.automation_store().unwrap();

    let first = land_and_evaluate(&fixture, &runtime, "covered landing\n").unwrap();
    let before = store.automation_state(&consumer).unwrap().unwrap();
    let active = before.active.as_ref().unwrap();
    let mut evidence = evidence_template(active);
    evidence.examination_complete = true;
    evidence.checks.push(ExaminationCheck {
        subject: first,
        method: "fixture examination".into(),
        observation: "complete".into(),
    });
    let bytes = serde_json::to_vec(&evidence).unwrap();
    let receipt = AcceptedCoverage {
        batch_id: active.batch.id.clone(),
        action_id: evidence.action_id.clone(),
        input_digest: active.input_digest.clone(),
        evidence_digest: orbit_common::security::release::sha256_hex(&bytes),
        evidence: bytes,
        evidence_reference: "fixture:accepted-examination".into(),
        submitted_by: "run:fixture-examiner".into(),
        accepted_at: Utc::now(),
    };
    let mut covered = before.clone();
    covered.generation += 1;
    covered.covered = active.batch.through_inclusive.clone();
    covered.pending.clear();
    covered.pending_commits.clear();
    covered.associations.clear();
    covered.active = None;
    assert!(
        store
            .automation_commit(&before, &covered, Some(&receipt))
            .unwrap()
    );
    // Close the first task so dedupe permits the next batch.
    fixture
        .command(&["task", "archive", &evidence.action_id])
        .assert()
        .success();

    let action_id = land_and_evaluate(&fixture, &runtime, "unpaid landing\n").unwrap();
    land_and_evaluate(&fixture, &runtime, "later unpaid landing\n");
    let before = store.automation_state(&consumer).unwrap().unwrap();
    assert_eq!(before.pending.len(), 2);
    assert_eq!(before.active.as_ref().unwrap().batch.deliveries.len(), 1);
    let mut claimed = before.clone();
    claimed.generation += 1;
    let attempt = claimed.active.as_mut().unwrap();
    attempt.state = BatchState::Claimed;
    if !checkpointed_id {
        attempt.action_id = None;
    }
    assert!(store.automation_commit(&before, &claimed, None).unwrap());
    (fixture, runtime, claimed, action_id)
}

fn retune(fixture: &Fixture, wait_minutes: u32) {
    let mut retuned = trigger();
    retuned["max_wait_minutes"] = json!(wait_minutes);
    retarget(fixture, &retuned);
}

fn assert_debt_unchanged(before: &AutomationState, after: &AutomationState) {
    let mut expected = before.clone();
    expected.generation = after.generation;
    expected.epoch = after.epoch.clone();
    expected.trigger = after.trigger.clone();
    expected.active = after.active.clone();
    assert_eq!(
        &expected, after,
        "every cursor and retained delivery fact survives"
    );
    assert_eq!(
        before.active.as_ref().unwrap().batch,
        after.active.as_ref().unwrap().batch
    );
    assert_eq!(
        before.active.as_ref().unwrap().input_digest,
        after.active.as_ref().unwrap().input_digest
    );
}

#[test]
fn terminal_claim_adopts_settings_and_doctor_reports_its_status() {
    const TEST: &str =
        "claimed_delivery_recovery::terminal_claim_adopts_settings_and_doctor_reports_its_status";
    if !in_isolated_child(TEST) {
        return;
    }
    for (checkpointed_id, status) in [
        (true, "archived"),
        (false, "archived"),
        (true, "rejected"),
        (false, "rejected"),
        (true, "done"),
        (false, "done"),
        (true, "deleted"),
    ] {
        let (fixture, runtime, before, action_id) = claimed(checkpointed_id);
        let store = runtime.automation_store().unwrap();
        let receipts = store.automation_receipts(&before.consumer, 100).unwrap();
        assert_eq!(receipts.len(), 1);
        match status {
            "archived" => {
                fixture
                    .command(&["task", "archive", &action_id])
                    .assert()
                    .success();
            }
            "deleted" => runtime.delete_task(&action_id).unwrap(),
            _ => {
                fixture.json(&[
                    "task", "update", &action_id, "--status", status, "--force", "--json",
                ]);
            }
        }
        let terminal = runtime.get_task(&action_id).ok();
        retune(&fixture, 30);
        let preview = fixture.json(&["auto-task", "recover", CONSUMER, "--json"]);
        assert_eq!(preview["action"]["action_id"], action_id);
        assert_eq!(preview["action"]["reissuable"], true);
        assert_eq!(preview["refusals"], json!([]));
        assert_eq!(
            store.automation_state(&before.consumer).unwrap().unwrap(),
            before
        );
        let (row, _) = doctor_row(&fixture);
        assert_eq!(row["status"], "error", "{row}");
        let message = row["message"].as_str().unwrap();
        assert!(message.contains(&format!("({status})")), "{message}");
        assert!(
            !message.contains(&action_id),
            "doctor output must not expose internal task IDs per AGENTS.md: {message}"
        );
        assert!(message.contains("orbit auto-task recover delivery-code-review --adopt-settings --reissue-action --reason"), "{message}");
        assert!(!message.contains("active_execution"), "{message}");
        fixture.json(&[
            "auto-task",
            "recover",
            CONSUMER,
            "--adopt-settings",
            "--reason",
            "retain the unpaid review batch",
            "--json",
        ]);
        let after = store.automation_state(&before.consumer).unwrap().unwrap();
        assert_ne!(after.epoch, before.epoch);
        assert_eq!(
            after.active, before.active,
            "adoption alone leaves the frozen attempt intact"
        );
        assert_debt_unchanged(&before, &after);
        assert_eq!(
            store.automation_receipts(&before.consumer, 100).unwrap(),
            receipts
        );
        assert_eq!(runtime.get_task(&action_id).ok(), terminal);
    }
}

#[test]
fn archived_claim_reissues_the_same_obligations_as_a_new_task() {
    const TEST: &str =
        "claimed_delivery_recovery::archived_claim_reissues_the_same_obligations_as_a_new_task";
    if !in_isolated_child(TEST) {
        return;
    }
    for checkpointed_id in [true, false] {
        let (fixture, runtime, before, action_id) = claimed(checkpointed_id);
        let store = runtime.automation_store().unwrap();
        let receipts = store.automation_receipts(&before.consumer, 100).unwrap();
        fixture
            .command(&["task", "archive", &action_id])
            .assert()
            .success();
        let archived = runtime.get_task(&action_id).unwrap();
        retune(&fixture, 30);
        let preview = fixture.json(&["auto-task", "recover", CONSUMER, "--json"]);
        assert_eq!(preview["action"]["reissuable"], true);
        fixture.json(&[
            "auto-task",
            "recover",
            CONSUMER,
            "--adopt-settings",
            "--reissue-action",
            "--reason",
            "re-examine the unpaid review batch",
            "--json",
        ]);
        let recovered = store.automation_state(&before.consumer).unwrap().unwrap();
        assert_debt_unchanged(&before, &recovered);
        let attempt = recovered.active.as_ref().unwrap();
        assert_eq!(attempt.attempt, before.active.as_ref().unwrap().attempt + 1);
        assert_eq!(
            attempt.reissue.as_ref().unwrap().from_action_id.as_deref(),
            Some(action_id.as_str())
        );
        let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
        let evaluated = evaluate_auto_task(&runtime, &definition, false, Utc::now())
            .unwrap()
            .state
            .unwrap();
        let new_attempt = evaluated.active.as_ref().unwrap();
        let new_id = new_attempt.action_id.as_ref().unwrap();
        assert_ne!(new_id, &action_id);
        assert_eq!(new_attempt.state, BatchState::Admitted);
        assert_debt_unchanged(&before, &evaluated);
        let task = runtime.get_task(new_id).unwrap();
        let frozen: Value = serde_json::from_str(
            task.description
                .split("```json\n")
                .nth(1)
                .unwrap()
                .split("```")
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            frozen["batch"],
            serde_json::to_value(&attempt.batch).unwrap()
        );
        assert!(task.description.contains(&action_id));
        assert_eq!(runtime.get_task(&action_id).unwrap(), archived);
        assert_eq!(
            store.automation_receipts(&before.consumer, 100).unwrap(),
            receipts
        );
    }
}

#[test]
fn scheduler_settles_closed_claims_and_adopts_but_refuses_open_tasks() {
    const TEST: &str = "claimed_delivery_recovery::scheduler_settles_closed_claims_and_adopts_but_refuses_open_tasks";
    if !in_isolated_child(TEST) {
        return;
    }
    for checkpointed_id in [true, false] {
        let (fixture, runtime, before, action_id) = claimed(checkpointed_id);
        let store = runtime.automation_store().unwrap();
        let receipts = store.automation_receipts(&before.consumer, 100).unwrap();
        retune(&fixture, 30);
        for status in ["proposed", "in-progress"] {
            if status == "in-progress" {
                fixture.json(&[
                    "task", "update", &action_id, "--status", status, "--force", "--json",
                ]);
            }
            let preview = fixture.json(&["auto-task", "recover", CONSUMER, "--json"]);
            assert_eq!(preview["refusals"], json!(["active_execution"]));
            assert_eq!(preview["action"]["reissuable"], false);
            fixture
                .command(&[
                    "auto-task",
                    "recover",
                    CONSUMER,
                    "--adopt-settings",
                    "--reason",
                    "must not interrupt a live task",
                ])
                .assert()
                .failure();
            let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
            let diagnostic =
                evaluate_auto_task(&runtime, &definition, !checkpointed_id, Utc::now()).unwrap();
            assert_eq!(diagnostic.reason, "definition_changed");
            assert_eq!(diagnostic.refusals, ["active_execution"]);
        }
        fixture
            .command(&["task", "archive", &action_id])
            .assert()
            .success();
        let archived = runtime.get_task(&action_id).unwrap();
        retune(&fixture, 15);
        let now = Utc::now();
        let tick = run_auto_task_scheduler_at(&runtime, now, SchedulerOptions::default()).unwrap();
        assert!(tick.errors.is_empty(), "{tick:?}");
        let report = tick
            .reports
            .iter()
            .find(|report| report.name == CONSUMER)
            .unwrap();
        let diagnostic = report.automation.as_ref().unwrap();
        assert_eq!(diagnostic.reason, "retry_backoff");
        let after = store.automation_state(&before.consumer).unwrap().unwrap();
        assert_ne!(after.epoch, before.epoch);
        assert_debt_unchanged(&before, &after);
        let active = after.active.as_ref().unwrap();
        assert_eq!(active.attempt, before.active.as_ref().unwrap().attempt + 1);
        assert!(active.action_id.is_none());
        assert!(active.retry_after.unwrap() > now);
        assert_eq!(doctor_row(&fixture).0["status"], "ok");
        assert_eq!(
            store.automation_receipts(&before.consumer, 100).unwrap(),
            receipts
        );
        assert_eq!(runtime.get_task(&action_id).unwrap(), archived);
        let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
        let resumed =
            evaluate_auto_task(&runtime, &definition, false, now + Duration::minutes(6)).unwrap();
        let new_id = resumed.state.unwrap().active.unwrap().action_id.unwrap();
        assert_ne!(
            new_id, action_id,
            "review resumes after the retained retry backoff"
        );
    }
}
