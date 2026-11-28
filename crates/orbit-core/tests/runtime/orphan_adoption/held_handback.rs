//! [ORB-23809] A delivery leaf that ended `held` on a delivery refusal a
//! fresh forge read can clear leaves its task in `review` with its pull
//! request open. An `--auto-fix` drain pass hands it back to the leaf's
//! completion step, as it does a failed one, and records
//! `pr_auto_fix_out_of_run`. A completion hold or `no-auto-approve` keeps it
//! back, a drain without `--auto-fix` never touches it, and a lineage that
//! holds again with the same code on the same head stops at a ceiling and is
//! then reported under its hold code.

use orbit_engine::RunLifecycle;
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
    orbit_core::test_support::EngineHost::new(&fx.runtime)
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
                    surface: None,
                    caller: None,
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
        fx.hold(&newest, 4, INCONCLUSIVE, "worker process exited");
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
    let head = "published-head";
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

    // Evidence from a replacement SHA on the same branch spends no hand-back
    // and starts no completion retry.
    let replacement = "replacement-head";
    let replacement_cases: [&dyn Fn(); 5] = [
        &|| fx.report_required_checks(&leaf, replacement, "orbit-bot", &[], &["Protected (rust)"]),
        &|| fx.report_required_checks(&leaf, replacement, "orbit-bot", &["Protected (rust)"], &[]),
        &|| fx.report_no_required_checks(&leaf, replacement, "orbit-bot"),
        &|| {
            fx.blocked(
                &leaf,
                replacement,
                "orbit-bot",
                false,
                &["Protected (rust)"],
            )
        },
        &|| fx.blocked(&leaf, replacement, "orbit-bot", false, &[]),
    ];
    for setup in replacement_cases {
        setup();
        let moved = fx.pass(&drain, "done", true);
        assert_eq!(
            repair_skip(&moved, &leaf.task).as_deref(),
            Some("publication_identity_changed"),
            "{moved}"
        );
        assert!(started(&moved, &leaf.task).is_none(), "{moved}");
        assert!(fx.retries(&leaf.run).is_empty(), "{moved}");
        assert!(out_of_run_records(&fx, &leaf.task).is_empty(), "{moved}");
    }
    let readiness = fx
        .runtime
        .task_pr_auto_fix_readiness(&leaf.task, true)
        .unwrap();
    assert_eq!(readiness["available"], false, "{readiness}");
    assert_eq!(
        readiness["reason"], "publication_identity_changed",
        "{readiness}"
    );
    match fx
        .runtime
        .start_task_pr_auto_fix(&leaf.task, true, Some("dashboard"))
    {
        Err(orbit_core::application::job::TaskAutoFixError::Refused(refusal)) => {
            assert_eq!(refusal.reason(), "publication_identity_changed");
        }
        other => panic!("expected publication_identity_changed refusal, got {other:?}"),
    }
    assert!(fx.retries(&leaf.run).is_empty());
    assert!(out_of_run_records(&fx, &leaf.task).is_empty());

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

    // Matching-head pending check evidence also allows the documented hand-back.
    let pending_leaf = fx.delivered_leaf(412, "done", Shelf::Review, LeafEnd::Held(code));
    fx.report_required_checks(&pending_leaf, head, "orbit-bot", &[], &["Protected (rust)"]);
    let pending_pass = fx.pass(&drain, "done", true);
    let pending_entry =
        started(&pending_pass, &pending_leaf.task).unwrap_or_else(|| panic!("{pending_pass}"));
    assert_eq!(pending_entry["hold_code"], code, "{pending_pass}");
    assert_eq!(pending_entry["blocker"], Value::Null, "{pending_pass}");
    assert_eq!(fx.retries(&pending_leaf.run).len(), 1, "{pending_pass}");

    // Missing published SHA fails closed as publication_identity_changed.
    let unrecorded = fx.delivered_leaf(413, "done", Shelf::Review, LeafEnd::Held(code));
    let mut unrecorded_state = fx.jobs.read_run_state(&unrecorded.run).unwrap().unwrap();
    unrecorded_state
        .pipeline
        .as_object_mut()
        .unwrap()
        .remove("push");
    fx.jobs
        .write_run_state(&unrecorded.run, &unrecorded_state)
        .unwrap();
    fx.report_required_checks(&unrecorded, head, "orbit-bot", &["Protected (rust)"], &[]);
    let missing_pass = fx.pass(&drain, "done", true);
    assert_eq!(
        repair_skip(&missing_pass, &unrecorded.task).as_deref(),
        Some("publication_identity_changed"),
        "{missing_pass}"
    );
    assert!(fx.retries(&unrecorded.run).is_empty(), "{missing_pass}");
}

fn auto_fix_stop_waits_for_green_on_published_head_then_resumes_completion() {
    let fx = fixture();
    // The step a real stop stores: the typed refusal `hold_auto_fix_stop`
    // gives it, with the `auto_fix_stopped` diagnostic in its message.
    let leaf = fx.delivered_leaf(10678, "done", Shelf::Review, LeafEnd::HeldAutoFixStop);
    // A skipped reviewer push may leave a null output; the initial push
    // remains the published head in that case.
    let mut state = fx.jobs.read_run_state(&leaf.run).unwrap().unwrap();
    state.record_pipeline_output("landing_push", Value::Null);
    fx.jobs.write_run_state(&leaf.run, &state).unwrap();
    let drain = fx.drain();
    fx.step(&drain, 0, "adopt_orphans", JobRunState::Success, None);
    fx.blocked(
        &leaf,
        "published-head",
        "orbit-bot",
        false,
        &["Test shard 13"],
    );
    let red = fx.pass(&drain, "done", true);
    assert_eq!(
        repair_skip(&red, &leaf.task).as_deref(),
        Some("auto_fix_stopped")
    );
    fx.report_required_checks(
        &leaf,
        "published-head",
        "orbit-bot",
        &["Check"],
        &["Test shard 13"],
    );
    let pending = fx.pass(&drain, "done", true);
    assert_eq!(
        repair_skip(&pending, &leaf.task).as_deref(),
        Some("auto_fix_stopped")
    );
    fx.report_required_checks(
        &leaf,
        "other-head",
        "orbit-bot",
        &["Check", "Test shard 13"],
        &[],
    );
    let moved = fx.pass(&drain, "done", true);
    assert_eq!(
        repair_skip(&moved, &leaf.task).as_deref(),
        Some("publication_identity_changed")
    );
    fx.report_required_checks(
        &leaf,
        "published-head",
        "orbit-bot",
        &["Check", "Test shard 13"],
        &[],
    );
    let green = fx.pass(&drain, "done", true);
    let entry = started(&green, &leaf.task).unwrap_or_else(|| panic!("{green}"));
    assert_eq!(entry["reason"], "checks_green_after_hold");
    let readiness =
        orbit_core::unit::workspace_auto::workspace_auto_readiness(&fx.runtime, &[], None, 10, &[])
            .unwrap();
    assert!(
        readiness["out_of_run_auto_fix"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["state"] == "handed_back"
                && entry["reason"] == "checks_green_after_hold"
                && entry["pr_number"] == "10678"),
        "{readiness}"
    );
    let retries = fx.retries(&leaf.run);
    assert_eq!(retries.len(), 1);
    let state = fx.jobs.read_run_state(&retries[0].run_id).unwrap().unwrap();
    assert_eq!(state.next_step_index, 4);
    assert_eq!(retries[0].input.as_ref().unwrap()["completion"], "done");
}

fn a_repo_with_no_required_checks_hands_back_check_runs_absent_hold() {
    let fx = fixture();
    let code = "delivery_check_runs_absent";
    let leaf = fx.delivered_leaf(411, "done", Shelf::Review, LeafEnd::Held(code));
    let head = "published-head";
    let drain = fx.drain();

    fx.report_no_required_checks(&leaf, "replacement-head", "orbit-bot");
    let moved = fx.pass(&drain, "done", true);
    assert_eq!(
        repair_skip(&moved, &leaf.task).as_deref(),
        Some("publication_identity_changed"),
        "{moved}"
    );
    assert!(fx.retries(&leaf.run).is_empty(), "{moved}");

    fx.report_no_required_checks(&leaf, head, "orbit-bot");
    let pass = fx.pass(&drain, "done", true);
    let entry = started(&pass, &leaf.task).unwrap_or_else(|| panic!("{pass}"));
    assert_eq!(entry["hold_code"], code, "{pass}");
    assert_eq!(entry["blocker"], Value::Null, "{pass}");
    assert_eq!(fx.retries(&leaf.run).len(), 1);
}

/// A stale required-checks answer under the unchanged published head holds as
/// inconclusive evidence, so matching red evidence on that head reaches the
/// repair ledger; a confirmed move to another head still repairs nothing.
fn a_stale_check_answer_hold_is_repaired_once_the_published_head_is_red() {
    let fx = fixture();
    let drain = fx.drain();
    let stale = fx.delivered_leaf(412, "done", Shelf::Review, LeafEnd::HeldStaleCheckAnswer);
    let moved = fx.delivered_leaf(413, "done", Shelf::Review, LeafEnd::HeldAutoFixStop);
    for leaf in [&stale, &moved] {
        fx.blocked(
            leaf,
            "published-head",
            "orbit-bot",
            false,
            &["Test shard 13"],
        );
    }
    let pass = fx.pass(&drain, "done", true);
    let entry = started(&pass, &stale.task).unwrap_or_else(|| panic!("{pass}"));
    assert_eq!(entry["blocker"], "failed_checks", "{pass}");
    assert_eq!(fx.retries(&stale.run).len(), 1, "{pass}");
    assert_eq!(
        repair_skip(&pass, &moved.task).as_deref(),
        Some("auto_fix_stopped"),
        "{pass}"
    );
    assert!(fx.retries(&moved.run).is_empty(), "{pass}");

    // A replacement head the hold does not own is never repaired. Each leaf
    // below is fresh with no live retry, so the verdict must read the forge
    // and reach the published-head identity guard instead of stopping at
    // `live_run` [ORB-28519].
    let red = fx.delivered_leaf(414, "done", Shelf::Review, LeafEnd::HeldStaleCheckAnswer);
    fx.blocked(
        &red,
        "replacement-head",
        "orbit-bot",
        false,
        &["Test shard 13"],
    );
    let conflicting = fx.delivered_leaf(415, "done", Shelf::Review, LeafEnd::HeldStaleCheckAnswer);
    fx.blocked(
        &conflicting,
        "replacement-head",
        "orbit-bot",
        true,
        &["Test shard 13"],
    );
    let other = fx.pass(&drain, "done", true);
    for leaf in [&red, &conflicting] {
        assert_eq!(
            repair_skip(&other, &leaf.task).as_deref(),
            Some("publication_identity_changed"),
            "{other}"
        );
        assert!(started(&other, &leaf.task).is_none(), "{other}");
        assert!(fx.retries(&leaf.run).is_empty(), "{other}");
        assert!(out_of_run_records(&fx, &leaf.task).is_empty(), "{other}");
        let readiness = fx
            .runtime
            .task_pr_auto_fix_readiness(&leaf.task, true)
            .unwrap();
        assert_eq!(readiness["available"], false, "{readiness}");
        assert_eq!(
            readiness["reason"], "publication_identity_changed",
            "{readiness}"
        );
        match fx
            .runtime
            .start_task_pr_auto_fix(&leaf.task, true, Some("dashboard"))
        {
            Err(orbit_core::application::job::TaskAutoFixError::Refused(refusal)) => {
                assert_eq!(refusal.reason(), "publication_identity_changed");
            }
            other => panic!("expected publication_identity_changed refusal, got {other:?}"),
        }
        assert!(fx.retries(&leaf.run).is_empty());
        assert!(out_of_run_records(&fx, &leaf.task).is_empty());
    }

    // An Orbit-audited clean base refresh of the published head remains
    // eligible: the same identity guard passes when the audit links the head.
    let audited_head = "audited-head";
    orbit_core::app::audit::record_audit_event(
        &fx.runtime,
        &orbit_store::contract::AuditEventInsertParams {
            execution_id: format!("audit-pr-refresh-{}", red.number),
            command: "pr_refresh".to_string(),
            subcommand: Some("updated".to_string()),
            tool_name: None,
            target_type: Some("pull_request".to_string()),
            target_id: Some(red.number.to_string()),
            role: "system".to_string(),
            status: orbit_types::telemetry::AuditEventStatus::Success,
            exit_code: 0,
            duration_ms: 0,
            working_directory: fx.runtime.paths().repo_root.to_string_lossy().into_owned(),
            arguments_json: Some(
                serde_json::json!({
                    "step": "pr_refresh",
                    "task_id": red.task,
                    "actor": "resident",
                    "pull_request": red.number,
                    "head_branch": red.head,
                    "base_branch": "main",
                    "head_sha": "published-head",
                    "new_head_sha": audited_head,
                })
                .to_string(),
            ),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: None,
            host: None,
            pid: std::process::id(),
            session_id: None,
            workspace_id: Some(fx.runtime.workspace_id().unwrap()),
            caller_machine_id: None,
            caller_machine_name: None,
            process_machine_id: None,
            process_machine_name: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: Some(red.task.clone()),
            job_run_id: None,
            activity_id: None,
            step_index: None,
        },
    )
    .unwrap();
    fx.blocked(&red, audited_head, "orbit-bot", false, &["Test shard 13"]);
    let audited = fx.pass(&drain, "done", true);
    let entry = started(&audited, &red.task).unwrap_or_else(|| panic!("{audited}"));
    assert_eq!(entry["blocker"], "failed_checks", "{audited}");
    assert_eq!(fx.retries(&red.run).len(), 1, "{audited}");
}

crate::scenario_table!(
    held_handback_scenarios,
    a_stale_check_answer_hold_is_repaired_once_the_published_head_is_red,
    auto_fix_stop_waits_for_green_on_published_head_then_resumes_completion,
    held_delivery_leaves_are_handed_back_to_completion,
    a_lineage_that_keeps_holding_on_the_same_head_stops_at_the_ceiling,
    a_check_runs_absent_hold_is_handed_back_once_the_head_reports_a_check,
    a_repo_with_no_required_checks_hands_back_check_runs_absent_hold,
);
