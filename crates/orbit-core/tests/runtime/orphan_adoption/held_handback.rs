//! [ORB-23809] A delivery leaf that ended `held` on a delivery refusal a
//! fresh forge read can clear leaves its task in `review` with its pull
//! request open. An `--auto-fix` drain pass hands it back to the leaf's
//! completion step, as it does a failed one, and records
//! `pr_auto_fix_out_of_run`. A completion hold or `no-auto-approve` keeps it
//! back, a drain without `--auto-fix` never touches it, and a lineage that
//! holds again with the same code on the same head stops at a ceiling and is
//! then reported under its hold code.

use orbit_types::task::{TaskDecision, TaskHistoryEntry};
use orbit_types::workflow::{COMPLETION_HELD_EVENT, CompletionHold, CompletionHoldField};

use super::out_of_run_auto_fix::{LeafEnd, Shelf, fixture, repair_skip, started};
use super::*;

const INCONCLUSIVE: &str = "delivery_check_evidence_inconclusive";

fn out_of_run_records(fx: &Fixture, task: &str) -> Vec<Value> {
    fx.runtime
        .get_task_history(task)
        .unwrap()
        .iter()
        .filter(|entry| entry.event == "pr_auto_fix_out_of_run")
        .map(|entry| serde_json::from_str(entry.note.as_deref().unwrap()).unwrap())
        .collect()
}

fn held_delivery_leaves_are_handed_back_to_completion() {
    let fx = fixture();
    let leaves = [
        (INCONCLUSIVE, 201),
        ("delivery_completion_budget_expired", 202),
        ("delivery_evidence_stale", 203),
    ]
    .map(|(code, number)| {
        let leaf = fx.delivered_leaf(number, "done", Shelf::Review, LeafEnd::Held(code));
        // The pull request reads clean: nothing for a repair, only for
        // completion to read again.
        fx.blocked(&leaf, &format!("{code}-head"), "orbit-bot", false, &[]);
        (code, leaf)
    });
    // A completion hold keeps its task in review for a person.
    let completion_held =
        fx.delivered_leaf(301, "done", Shelf::Review, LeafEnd::Held(INCONCLUSIVE));
    fx.blocked(&completion_held, "hold-head", "orbit-bot", false, &[]);
    fx.runtime
        .apply_task_automation_update(
            &completion_held.task,
            TaskAutomationUpdate {
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: COMPLETION_HELD_EVENT.into(),
                    note: Some("held for review".into()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: Some(TaskDecision::CompletionHeld(CompletionHold {
                        rule: "risky".into(),
                        field: CompletionHoldField::Tags,
                        pattern: "risky".into(),
                        matched: "risky".into(),
                        matched_task_id: None,
                        run_id: completion_held.run.clone(),
                    })),
                }],
                ..Default::default()
            },
        )
        .unwrap();
    let opted_out = fx.delivered_leaf(302, "done", Shelf::OptedOut, LeafEnd::Held(INCONCLUSIVE));
    fx.blocked(&opted_out, "opted-head", "orbit-bot", false, &[]);
    let drain = fx.drain();

    // A drain started without `--auto-fix` hands back no held leaf.
    let plain = fx.pass(&drain, "done", false);
    assert!(plain.get("auto_fix").is_none(), "{plain}");
    for (_, leaf) in &leaves {
        assert!(fx.retries(&leaf.run).is_empty(), "{plain}");
    }

    let pass = fx.pass(&drain, "done", true);
    for (code, leaf) in &leaves {
        let entry = started(&pass, &leaf.task).unwrap_or_else(|| panic!("{code}: {pass}"));
        assert_eq!(entry["hold_code"], *code, "{pass}");
        assert_eq!(entry["blocker"], Value::Null, "{pass}");
        let retries = fx.retries(&leaf.run);
        assert_eq!(retries.len(), 1, "{code}: the held leaf is resumed once");
        let input = retries[0].input.as_ref().unwrap();
        assert_eq!(input["auto_fix"], true, "{input}");
        assert_eq!(
            input["completion"], "done",
            "the hand-back keeps the leaf's own completion"
        );
        assert_eq!(input["out_of_run_auto_fix"]["hold_code"], *code, "{input}");
        let seeded = fx.jobs.read_run_state(&retries[0].run_id).unwrap().unwrap();
        assert_eq!(seeded.next_step_index, 4, "{code}: completion reruns");
        let records = out_of_run_records(&fx, &leaf.task);
        assert_eq!(records.len(), 1, "{code}: {records:?}");
        assert_eq!(records[0]["hold_code"], *code);
        assert_eq!(records[0]["resumed_run_id"], retries[0].run_id);
        assert_eq!(
            fx.runtime.get_task(&leaf.task).unwrap().status,
            TaskStatus::Review
        );
    }
    assert_eq!(
        repair_skip(&pass, &completion_held.task).as_deref(),
        Some("completion_held"),
        "{pass}"
    );
    assert_eq!(
        repair_skip(&pass, &opted_out.task).as_deref(),
        Some("opted_out"),
        "{pass}"
    );
    assert!(fx.retries(&completion_held.run).is_empty());
    assert!(fx.retries(&opted_out.run).is_empty());
    // The source leaf stays held: it is not recounted as a failure.
    for (_, leaf) in &leaves {
        assert_eq!(
            fx.jobs.get_job_run(&leaf.run).unwrap().unwrap().state,
            JobRunState::Held
        );
    }
}

fn a_lineage_that_keeps_holding_on_the_same_head_stops_at_the_ceiling() {
    let fx = fixture();
    let leaf = fx.delivered_leaf(310, "done", Shelf::Review, LeafEnd::Held(INCONCLUSIVE));
    fx.blocked(&leaf, "same-head", "orbit-bot", false, &[]);
    // Fix PR offers the same hand-back.
    let readiness = fx
        .runtime
        .task_pr_auto_fix_readiness(&leaf.task, true)
        .unwrap();
    assert_eq!(readiness["available"], true, "{readiness}");
    assert_eq!(readiness["hold_code"], INCONCLUSIVE, "{readiness}");
    let drain = fx.drain();
    let mut newest = leaf.run.clone();
    for hand_back in 1..=3 {
        let pass = fx.pass(&drain, "done", true);
        assert!(
            started(&pass, &leaf.task).is_some(),
            "hand-back {hand_back}: {pass}"
        );
        let retries = fx.retries(&newest);
        assert_eq!(retries.len(), 1, "hand-back {hand_back}");
        newest = retries[0].run_id.clone();
        // The resumed completion holds again for the same reason on the
        // same head; the fixture backdates it past the backoff.
        fx.hold(&newest, 4, INCONCLUSIVE);
    }
    let pass = fx.pass(&drain, "done", true);
    assert_eq!(
        repair_skip(&pass, &leaf.task).as_deref(),
        Some(INCONCLUSIVE),
        "the spent lineage is reported under its hold code: {pass}"
    );
    assert!(fx.retries(&newest).is_empty(), "{pass}");
    assert_eq!(out_of_run_records(&fx, &leaf.task).len(), 3);
    match fx
        .runtime
        .start_task_pr_auto_fix(&leaf.task, true, Some("dashboard"))
    {
        Err(orbit_core::application::job::TaskAutoFixError::Refused(refusal)) => {
            assert_eq!(refusal.reason(), INCONCLUSIVE);
        }
        other => panic!("expected the ceiling's refusal, got {other:?}"),
    }

    // A new head is a new lineage position.
    fx.blocked(&leaf, "new-head", "orbit-bot", false, &[]);
    let pass = fx.pass(&drain, "done", true);
    assert!(started(&pass, &leaf.task).is_some(), "{pass}");
}

fn a_check_runs_absent_hold_is_handed_back_once_the_head_reports_a_check() {
    let fx = fixture();
    let code = "delivery_check_runs_absent";
    let leaf = fx.delivered_leaf(410, "done", Shelf::Review, LeafEnd::Held(code));
    let head = "absent-head";
    fx.blocked(&leaf, head, "orbit-bot", false, &[]);
    let drain = fx.drain();

    // No required check run yet: the hold stays visible and spends nothing.
    let waiting = fx.pass(&drain, "done", true);
    let skip = waiting["auto_fix"]["skipped"]
        .as_array()
        .and_then(|skipped| skipped.iter().find(|entry| entry["task_id"] == leaf.task))
        .unwrap_or_else(|| panic!("{waiting}"));
    assert_eq!(skip["reason"], code, "{skip}");
    assert_eq!(skip["hold_code"], code, "{skip}");
    assert!(
        skip["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("no required check run")),
        "{skip}"
    );
    assert!(started(&waiting, &leaf.task).is_none(), "{waiting}");
    assert!(fx.retries(&leaf.run).is_empty(), "{waiting}");
    assert!(out_of_run_records(&fx, &leaf.task).is_empty(), "{waiting}");

    // The same head later reports a required check. Completion is handed
    // the pull request; the source leaf stays held and the task stays in review.
    fx.report_required_checks(&leaf, head, "orbit-bot", &["Protected (rust)"], &[]);
    let pass = fx.pass(&drain, "done", true);
    let entry = started(&pass, &leaf.task).unwrap_or_else(|| panic!("{pass}"));
    assert_eq!(entry["hold_code"], code, "{pass}");
    assert_eq!(entry["blocker"], Value::Null, "{pass}");
    let retries = fx.retries(&leaf.run);
    assert_eq!(retries.len(), 1, "{code}: the held leaf is resumed once");
    let input = retries[0].input.as_ref().unwrap();
    assert_eq!(input["out_of_run_auto_fix"]["hold_code"], code, "{input}");
    assert_eq!(input["out_of_run_auto_fix"]["head_sha"], head, "{input}");
    let seeded = fx.jobs.read_run_state(&retries[0].run_id).unwrap().unwrap();
    assert_eq!(seeded.next_step_index, 4, "completion reruns");
    let records = out_of_run_records(&fx, &leaf.task);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0]["hold_code"], code);
    assert_eq!(
        fx.runtime.get_task(&leaf.task).unwrap().status,
        TaskStatus::Review
    );
    assert_eq!(
        fx.jobs.get_job_run(&leaf.run).unwrap().unwrap().state,
        JobRunState::Held
    );
}

crate::scenario_table!(
    held_handback_scenarios,
    held_delivery_leaves_are_handed_back_to_completion,
    a_lineage_that_keeps_holding_on_the_same_head_stops_at_the_ceiling,
    a_check_runs_absent_hold_is_handed_back_once_the_head_reports_a_check,
);
