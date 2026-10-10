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
    // Stored as version 1 evidence, accepted before per-delivery examination
    // was required.
    let mut stored = serde_json::to_value(&evidence).unwrap();
    stored["schema_version"] = json!(1);
    stored
        .as_object_mut()
        .unwrap()
        .remove("delivery_examinations");
    let bytes = serde_json::to_vec(&stored).unwrap();
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
fn unminted_retry_recovers_after_a_legacy_task_closed_and_settings_changed() {
    const TEST: &str = "claimed_delivery_recovery::unminted_retry_recovers_after_a_legacy_task_closed_and_settings_changed";
    if !in_isolated_child(TEST) {
        return;
    }
    for status in ["archived", "rejected", "done", "deleted"] {
        let (fixture, runtime, legacy, action_id) = claimed(false);
        let store = runtime.automation_store().unwrap();
        let receipts = store.automation_receipts(&legacy.consumer, 100).unwrap();
        if matches!(status, "archived" | "deleted") {
            fixture
                .command(&["task", "archive", &action_id])
                .assert()
                .success();
        } else {
            fixture.json(&[
                "task", "update", &action_id, "--status", status, "--force", "--json",
            ]);
        }
        let closed = runtime.get_task(&action_id).unwrap();
        let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
        let before = evaluate_auto_task(&runtime, &definition, false, Utc::now())
            .unwrap()
            .state
            .unwrap();
        assert_debt_unchanged(&legacy, &before);
        let active = before.active.as_ref().unwrap();
        assert_eq!(active.attempt, 2);
        assert_eq!(active.state, BatchState::Claimed);
        assert!(active.action_id.is_none());
        assert!(active.retry_after.is_some());
        assert_eq!(
            active.reason.as_deref(),
            Some("task_closed_without_accepted_evidence")
        );
        if status == "deleted" {
            runtime.delete_task(&action_id).unwrap();
        }

        // [ORB-14579] The old task belongs to attempt 1. A later definition
        // edit must not mistake its unminted retry for executing work.
        retune(&fixture, 30);
        let preview = fixture.json(&["auto-task", "recover", CONSUMER, "--json"]);
        if status == "deleted" {
            assert_eq!(preview["refusals"], json!(["active_execution"]));
            assert_eq!(
                preview["action"]["reissuable"], false,
                "a backoff without a resolvable predecessor is not terminal proof"
            );
            fixture
                .command(&[
                    "auto-task",
                    "recover",
                    CONSUMER,
                    "--adopt-settings",
                    "--reason",
                    "must retain unknown liveness",
                ])
                .assert()
                .failure();
            assert_eq!(
                store.automation_state(&before.consumer).unwrap().unwrap(),
                before
            );
            assert_eq!(
                store.automation_receipts(&before.consumer, 100).unwrap(),
                receipts
            );
            continue;
        }
        assert_eq!(
            preview["refusals"],
            json!([]),
            "unminted retry after {status}: {preview}"
        );
        assert_eq!(preview["action"]["reissuable"], true);
        assert_eq!(
            store.automation_state(&before.consumer).unwrap().unwrap(),
            before
        );
        fixture.json(&[
            "auto-task",
            "recover",
            CONSUMER,
            "--adopt-settings",
            "--reason",
            "retain the legacy retry debt",
            "--json",
        ]);
        let adopted = store.automation_state(&before.consumer).unwrap().unwrap();
        assert_ne!(adopted.epoch, before.epoch);
        assert_eq!(adopted.active, before.active);
        assert_debt_unchanged(&before, &adopted);
        assert_eq!(doctor_row(&fixture).0["status"], "ok");

        // Explicit reissue is audited and retains the entire frozen batch,
        // even though this retry never minted an action of its own.
        let reissued = fixture.json(&[
            "auto-task",
            "recover",
            CONSUMER,
            "--reissue-action",
            "--reason",
            "authorize another examination",
            "--json",
        ]);
        assert_eq!(reissued["applied"], json!(["reissued_action"]));
        assert_eq!(
            reissued["action"]["reissuable"], false,
            "a fresh operator claim has no settled action of its own"
        );
        let after = store.automation_state(&before.consumer).unwrap().unwrap();
        assert_eq!(after.active.as_ref().unwrap().attempt, 3);
        assert_debt_unchanged(&before, &after);
        assert_eq!(
            store.automation_receipts(&before.consumer, 100).unwrap(),
            receipts
        );
        assert_eq!(runtime.get_task(&action_id).unwrap(), closed);
        assert_eq!(
            store
                .automation_recoveries(&before.consumer, 10)
                .unwrap()
                .len(),
            2
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
        // Another edit on a later tick must retain the same terminal proof;
        // retry_scheduled is only true during the original settlement pass.
        retune(&fixture, 45);
        let preview = fixture.json(&["auto-task", "recover", CONSUMER, "--json"]);
        assert_eq!(preview["refusals"], json!([]));
        assert_eq!(preview["action"]["reissuable"], true);
        let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
        let resumed =
            evaluate_auto_task(&runtime, &definition, false, now + Duration::minutes(6)).unwrap();
        let resumed_state = resumed.state.unwrap();
        assert_ne!(resumed_state.epoch, after.epoch);
        assert_debt_unchanged(&before, &resumed_state);
        let new_id = resumed_state.active.unwrap().action_id.unwrap();
        assert_ne!(
            new_id, action_id,
            "review resumes after the retained retry backoff"
        );
        retune(&fixture, 60);
        let preview = fixture.json(&["auto-task", "recover", CONSUMER, "--json"]);
        assert_eq!(preview["refusals"], json!(["active_execution"]));
        assert_eq!(
            preview["action"]["reissuable"], false,
            "a live retry overrides its predecessor's failure"
        );
    }
}

/// A receipt accepted under evidence schema 1 stays settled and still reads
/// as the task's accepted coverage [ORB-15186].
#[test]
fn version_one_receipts_still_read_as_accepted_coverage() {
    const TEST: &str =
        "claimed_delivery_recovery::version_one_receipts_still_read_as_accepted_coverage";
    if !in_isolated_child(TEST) {
        return;
    }
    let (_fixture, runtime, before, _) = claimed(true);
    let receipts = runtime
        .automation_store()
        .unwrap()
        .automation_receipts(&before.consumer, 100)
        .unwrap();
    assert_eq!(receipts.len(), 1);
    let evidence =
        orbit_engine::RuntimeHost::accepted_automation_coverage(&runtime, &receipts[0].action_id)
            .unwrap()
            .expect("version 1 coverage reads");
    assert_eq!(evidence.schema_version, 1);
    assert!(evidence.delivery_examinations.is_empty());
    assert!(evidence.examination_complete);
}
