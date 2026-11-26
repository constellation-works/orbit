use super::*;
use orbit_types::workflow::{RESIDENT_CYCLE_CHANGED_EVENT, ResidentLiveCycle};

/// Seeds one cycle-change row at `at` with its note taken verbatim, as an
/// earlier build or a damaged row wrote it.
fn seed_cycle_change(fx: &Fixture, mandate: &str, at: DateTime<Utc>, note: &str) {
    fx.runtime
        .stores()
        .task_history()
        .update_task_history(
            mandate,
            orbit_store::contract::TaskHistoryUpdateParams {
                actor: "fixture".into(),
                append_history: vec![TaskHistoryEntry {
                    at,
                    by: "system".into(),
                    event: RESIDENT_CYCLE_CHANGED_EVENT.into(),
                    note: Some(note.into()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                    surface: None,
                    caller: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();
}

/// The note of the newest cycle-change row.
fn latest_cycle_note(fx: &Fixture, mandate: &str) -> String {
    fx.runtime
        .get_task_history(mandate)
        .unwrap()
        .iter()
        .rev()
        .find(|entry| entry.event == RESIDENT_CYCLE_CHANGED_EVENT)
        .and_then(|entry| entry.note.clone())
        .unwrap()
}

#[test]
fn live_cycle_history_keeps_its_encoding_and_reads_fail_closed_or_tolerant() {
    if !crate::dispatch_admission::isolated(
        "resident_cycle::checkpoint_history::live_cycle_history_keeps_its_encoding_and_reads_fail_closed_or_tolerant",
    ) {
        return;
    }
    let fx = fixture();
    let (_watched, mandate) = fx.watched_mandate("Typed live cycle history");
    let now = Utc::now();
    let lease = |cycle: &str| ResidentLiveCycle {
        cycle: cycle.into(),
        run_id: format!("run-{cycle}"),
        expires_at: now + Duration::hours(1),
    };

    // The previous start object, 84 hours ago: the earliest readable start.
    let old = lease("cycle-old");
    seed_cycle_change(
        &fx,
        &mandate,
        now - Duration::hours(84),
        &serde_json::to_string(&old).unwrap(),
    );

    // The previous start is the live cycle: a stale expectation is refused and
    // the matching one clears it, leaving the clear as JSON `null`.
    let stale = fx
        .runtime
        .set_resident_live_cycle(&mandate, None, None)
        .unwrap_err()
        .to_string();
    assert!(stale.contains("resident_cycle_stale"), "{stale}");
    fx.runtime
        .set_resident_live_cycle(&mandate, Some("cycle-old"), None)
        .unwrap();
    assert_eq!(latest_cycle_note(&fx, &mandate), "null");

    // A publication stores the lease object as its note; a reused id is refused
    // across the clear.
    let fresh = lease("cycle-fresh");
    fx.runtime
        .set_resident_live_cycle(&mandate, None, Some(&fresh))
        .unwrap();
    assert_eq!(
        latest_cycle_note(&fx, &mandate),
        serde_json::to_string(&fresh).unwrap()
    );
    fx.runtime
        .set_resident_live_cycle(&mandate, Some("cycle-fresh"), None)
        .unwrap();
    let reused = fx
        .runtime
        .set_resident_live_cycle(&mandate, None, Some(&old))
        .unwrap_err()
        .to_string();
    assert!(reused.contains("cannot be reused"), "{reused}");

    // A malformed latest change fails closed: neither the current-live read
    // nor a publication may proceed past it.
    seed_cycle_change(&fx, &mandate, now - Duration::hours(23), "{\"cycle\":");
    for next in [None, Some(&lease("cycle-blocked"))] {
        let unreadable = fx
            .runtime
            .set_resident_live_cycle(&mandate, None, next)
            .unwrap_err()
            .to_string();
        assert!(
            unreadable.contains("resident cycle state is unreadable"),
            "{unreadable}"
        );
    }

    // Behind a valid clear the malformed row is skipped: the reuse scan still
    // refuses the old id and admits a fresh one.
    seed_cycle_change(&fx, &mandate, now - Duration::hours(22), "null");
    let reused = fx
        .runtime
        .set_resident_live_cycle(&mandate, None, Some(&old))
        .unwrap_err()
        .to_string();
    assert!(reused.contains("cannot be reused"), "{reused}");
    let next = lease("cycle-next");
    fx.runtime
        .set_resident_live_cycle(&mandate, None, Some(&next))
        .unwrap();
    assert_eq!(
        latest_cycle_note(&fx, &mandate),
        serde_json::to_string(&next).unwrap()
    );

    // Activation is the earliest readable start, so the mandate has been active
    // for 84 hours (4 whole days), not since the latest start.
    fx.runtime
        .set_resident_live_cycle(&mandate, Some("cycle-next"), None)
        .unwrap();
    let report = fx
        .runtime
        .resident_metrics(Some(&mandate), None, None)
        .unwrap();
    let entry = report["mandates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == mandate.as_str())
        .unwrap_or_else(|| panic!("mandate missing from report: {report}"));
    assert_eq!(entry["active_days"], 4.0, "{report}");
}

#[test]
fn checkpoint_notes_refuse_non_delta_length_without_losing_legacy_history() {
    if !crate::dispatch_admission::isolated(
        "resident_cycle::checkpoint_history::checkpoint_notes_refuse_non_delta_length_without_losing_legacy_history",
    ) {
        return;
    }
    let fx = fixture();
    let (watched, mandate) = fx.watched_mandate("Bound checkpoint memory");
    let now = Utc::now();
    let run = fx.cycle_run(&mandate, "resident-notes");
    let brief = fx.begin_cycle(&run, now);
    let written = fx.write_checkpoint(&brief, &run, json!({"notes": "é".repeat(600)}));
    fx.finish_cycle(&run);

    fx.update_task(
        &watched,
        TaskUpdateParams {
            comment: Some("Cycle delta arrived".into()),
            ..Default::default()
        },
    );
    let run = fx.cycle_run(&mandate, "resident-notes-next");
    let brief = fx.begin_cycle(&run, now + Duration::minutes(15));
    assert_eq!(brief["cycle_count"], 1, "{brief}");
    let mut record = written["checkpoint"].clone();
    record["cycle"] = brief["cycle"].clone();
    record["run_id"] = json!(run);
    let before = fx.runtime.get_task_comments(&mandate).unwrap().len();
    for length in [601, 2001] {
        record["notes"] = json!("é".repeat(length));
        for tool in ["orbit.resident.checkpoint", "orbit.task.update"] {
            let input = if tool == "orbit.resident.checkpoint" {
                json!({"task_id": mandate, "record": record})
            } else {
                json!({"id": mandate, "comment": record.to_string()})
            };
            let error = call(&fx.runtime, tool, input, McpCapability::Agent).unwrap_err();
            assert!(
                error.contains("600") && error.contains("delta"),
                "{tool}: {error}"
            );
            assert_eq!(
                fx.runtime.get_task_comments(&mandate).unwrap().len(),
                before
            );
        }
    }
    fx.write_checkpoint(&brief, &run, json!({"notes": "é".repeat(600)}));
    fx.finish_cycle(&run);

    // Seed records accepted by older versions, bypassing current write admission.
    let legacy_notes = "古".repeat(2000);
    let mut legacy = written["checkpoint"].clone();
    legacy["notes"] = json!(legacy_notes);
    legacy["conversation_ref"] = json!("legacy-conversation");
    for index in 0..30 {
        legacy["cycle"] = json!(format!("legacy-cycle-{index}"));
        legacy["run_id"] = json!(format!("legacy-run-{index}"));
        fx.runtime
            .stores()
            .task_history()
            .update_task_history(
                &mandate,
                orbit_store::contract::TaskHistoryUpdateParams {
                    actor: "fixture".into(),
                    append_comments: vec![orbit_types::task::TaskComment {
                        at: now,
                        by: "fixture".into(),
                        message: legacy.to_string(),
                        operation_id: None,
                    }],
                    ..Default::default()
                },
            )
            .unwrap();
    }
    fx.update_task(
        &watched,
        TaskUpdateParams {
            comment: Some("Read historical memory".into()),
            ..Default::default()
        },
    );
    let run = fx.cycle_run(&mandate, "resident-legacy-history");
    let brief = fx.begin_cycle(&run, now + Duration::minutes(30));
    assert_eq!(brief["cycle_count"], 1, "{brief}");
    assert_eq!(brief["checkpoints"].as_array().unwrap().len(), 1);
    assert_eq!(brief["checkpoints"][0]["notes"], legacy_notes);
    assert_eq!(brief["checkpoints"][0]["cycle"], "legacy-cycle-29");
    assert_eq!(brief["checkpoints"][0]["conversation_ref"], Value::Null);
    let summaries = brief["earlier_checkpoint_summaries"].as_array().unwrap();
    assert_eq!(
        summaries.len(),
        orbit_types::workflow::RESIDENT_BRIEF_CHECKPOINTS - 1
    );
    for (index, summary) in summaries.iter().enumerate() {
        assert_eq!(summary["cycle"], format!("legacy-cycle-{}", 28 - index));
        assert_eq!(summary["notes"].as_str().unwrap().chars().count(), 600);
        assert!(summary.get("conversation_ref").is_none());
    }
    assert!(brief.get("older_checkpoint_notes").is_none());
    assert!(
        brief["comments_since_last_cycle"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn agent_checkpoint_refuses_the_system_heartbeat_skip_outcome() {
    if !crate::dispatch_admission::isolated(
        "resident_cycle::checkpoint_history::agent_checkpoint_refuses_the_system_heartbeat_skip_outcome",
    ) {
        return;
    }
    let fx = fixture();
    let (watched, mandate) = fx.watched_mandate("Reserve the heartbeat skip");
    let now = Utc::now();
    let run = fx.cycle_run(&mandate, "resident-reserved-outcome");
    let brief = fx.begin_cycle(&run, now);
    let written = fx.write_checkpoint(&brief, &run, json!({"outcome": "idle"}));
    fx.finish_cycle(&run);

    fx.update_task(
        &watched,
        TaskUpdateParams {
            comment: Some("Cycle delta arrived".into()),
            ..Default::default()
        },
    );
    let run = fx.cycle_run(&mandate, "resident-reserved-outcome-next");
    let brief = fx.begin_cycle(&run, now + Duration::minutes(15));
    let mut record = written["checkpoint"].clone();
    record["cycle"] = brief["cycle"].clone();
    record["run_id"] = json!(run);
    record["outcome"] = json!("unchanged");
    let before = fx.runtime.get_task_comments(&mandate).unwrap().len();
    for tool in ["orbit.resident.checkpoint", "orbit.task.update"] {
        let input = if tool == "orbit.resident.checkpoint" {
            json!({"task_id": mandate, "record": record})
        } else {
            json!({"id": mandate, "comment": record.to_string()})
        };
        let error = call(&fx.runtime, tool, input, McpCapability::Agent).unwrap_err();
        assert!(error.contains("reserved"), "{tool}: {error}");
        assert_eq!(
            fx.runtime.get_task_comments(&mandate).unwrap().len(),
            before
        );
    }
    // The same cycle still writes its one real checkpoint.
    fx.write_checkpoint(&brief, &run, json!({"outcome": "idle"}));
}

#[test]
fn invalid_checkpoint_can_be_corrected_before_single_accepted_write() {
    if !crate::dispatch_admission::isolated(
        "resident_cycle::checkpoint_history::invalid_checkpoint_can_be_corrected_before_single_accepted_write",
    ) {
        return;
    }
    let fx = fixture();
    let (_watched, mandate) = fx.watched_mandate("Repair a refused checkpoint");
    let run = fx.cycle_run(&mandate, "resident-checkpoint-retry");
    let brief = fx.begin_cycle(&run, Utc::now());
    let cycle = brief["cycle"].as_str().unwrap().to_owned();
    let mut record = json!({
        "kind": "resident.checkpoint",
        "schema": 1,
        "mandate": mandate,
        "cycle": cycle,
        "run_id": run,
        "crew": "sol",
        "recovery_of": brief["recovery_of"],
        "wake": brief["wake"],
        "authority": {
            "rights": brief["authority"]["rights"],
            "observe_only": brief["authority"]["observe_only"],
            "narrowed_by": brief["authority"]["narrowed_by"]
        },
        "actions": [],
        "decisions": [],
        "requests": [],
        "costs": [{
            "kind": "manual_step",
            "what": "Checked the checkpoint schema",
            "minutes": 1,
            "retries": 0,
            "slot_minutes": 0,
            "signature": "manual-schema-check"
        }],
        "notes": "Correct the rejected cost and record the cycle.",
        "next_wake": {
            "reason": "heartbeat",
            "not_before": Utc::now() + Duration::hours(2)
        },
        "continuity": "rebuilt",
        "outcome": "progress",
        "probe": false
    });
    let comments_before = fx.runtime.get_task_comments(&mandate).unwrap().len();
    let accepted_for_cycle = || {
        fx.runtime
            .get_task_comments(&mandate)
            .unwrap()
            .iter()
            .filter_map(|comment| ResidentComment::parse(&comment.message).ok())
            .filter(|record| {
                matches!(record, ResidentComment::Checkpoint(checkpoint) if checkpoint.cycle == cycle)
            })
            .count()
    };

    let error = call(
        &fx.runtime,
        "orbit.resident.checkpoint",
        json!({"task_id": mandate, "record": record}),
        McpCapability::Agent,
    )
    .unwrap_err();
    assert!(
        error.contains("resident_record_invalid") && error.contains("costs.wished"),
        "unexpected refusal: {error}"
    );
    assert_eq!(
        fx.runtime.get_task_comments(&mandate).unwrap().len(),
        comments_before
    );
    assert_eq!(
        accepted_for_cycle(),
        0,
        "validation refusal wrote no checkpoint"
    );

    record["costs"][0]["wished"] = json!("Automatic checkpoint schema validation");
    let accepted = call(
        &fx.runtime,
        "orbit.resident.checkpoint",
        json!({"task_id": mandate, "record": record.clone()}),
        McpCapability::Agent,
    )
    .unwrap();
    assert_eq!(accepted["checkpoint"], record);
    assert_eq!(
        accepted_for_cycle(),
        1,
        "corrected checkpoint was accepted once"
    );

    let duplicate = call(
        &fx.runtime,
        "orbit.resident.checkpoint",
        json!({"task_id": mandate, "record": record}),
        McpCapability::Agent,
    )
    .unwrap_err();
    assert!(
        duplicate.contains("resident_checkpoint_exists"),
        "{duplicate}"
    );
    assert_eq!(
        accepted_for_cycle(),
        1,
        "duplicate refusal added no checkpoint"
    );
    fx.finish_cycle(&run);
}

#[test]
fn rejected_checkpoint_surfaces_cause_and_field_on_exited_and_unknown_cycles() {
    if !crate::dispatch_admission::isolated(
        "resident_cycle::checkpoint_history::rejected_checkpoint_surfaces_cause_and_field_on_exited_and_unknown_cycles",
    ) {
        return;
    }
    let fx = fixture();
    let (watched, mandate) = fx.watched_mandate("Preserve rejected checkpoint cause");
    let now = Utc::now();

    let checkpoints_for_cycle = |cycle: &str| -> Vec<orbit_types::workflow::ResidentCheckpoint> {
        fx.runtime
            .get_task_comments(&mandate)
            .unwrap()
            .iter()
            .filter_map(
                |comment| match ResidentComment::parse(&comment.message).ok()? {
                    ResidentComment::Checkpoint(checkpoint) if checkpoint.cycle == cycle => {
                        Some(*checkpoint)
                    }
                    _ => None,
                },
            )
            .collect()
    };

    // Case 1: Live cycle performs an action, hits `resident_record_invalid` on
    // `costs.wished`, then exits without retrying and is recovered by the next
    // cycle's `observe`. The synthesized incomplete checkpoint surfaces the
    // validation cause and field, preserves the earlier action receipt, and the
    // recovery cycle replays the action as `already_done` without repeating it.
    let dead_run = fx.cycle_run(&mandate, "resident-exited-invalid-checkpoint");
    let dead_brief = fx.begin_cycle(&dead_run, now);
    let dead_cycle = dead_brief["cycle"].as_str().unwrap().to_owned();
    let comment_input = json!({"id": watched, "comment": "Checked CI.", "model": "codex"});
    let action_key = fx.as_cycle_agent(&dead_run, || {
        fx.agent_call("orbit.task.update", comment_input.clone())
            .unwrap()["resident_action_key"]
            .as_str()
            .unwrap()
            .to_owned()
    });
    let watched_comments_after_action = fx.runtime.get_task_comments(&watched).unwrap().len();
    let invalid_record = json!({
        "kind": "resident.checkpoint",
        "schema": 1,
        "mandate": mandate,
        "cycle": dead_cycle,
        "run_id": dead_run,
        "crew": "sol",
        "recovery_of": dead_brief["recovery_of"],
        "wake": dead_brief["wake"],
        "authority": {
            "rights": dead_brief["authority"]["rights"],
            "observe_only": dead_brief["authority"]["observe_only"],
            "narrowed_by": dead_brief["authority"]["narrowed_by"]
        },
        "actions": [{
            "key": action_key,
            "kind": "orbit.task.update",
            "subject": watched,
            "result": "ok",
            "ref": watched
        }],
        "decisions": [],
        "requests": [],
        "costs": [{
            "kind": "manual_step",
            "what": "Manually inspected CI status",
            "minutes": 1,
            "retries": 0,
            "slot_minutes": 0,
            "signature": "manual-ci-check"
        }],
        "notes": "Action recorded before checkpoint failed validation.",
        "next_wake": {
            "reason": "heartbeat",
            "not_before": now + Duration::hours(2)
        },
        "continuity": "rebuilt",
        "outcome": "progress",
        "probe": false
    });
    let refusal = fx.as_cycle_agent(&dead_run, || {
        call(
            &fx.runtime,
            "orbit.resident.checkpoint",
            json!({"task_id": mandate, "record": invalid_record}),
            McpCapability::Agent,
        )
        .unwrap_err()
    });
    assert!(
        refusal.contains("resident_record_invalid") && refusal.contains("costs.wished"),
        "{refusal}"
    );
    assert!(
        checkpoints_for_cycle(&dead_cycle).is_empty(),
        "validation refusal wrote no checkpoint"
    );
    let run_state = fx.runtime.read_run_state(&dead_run).unwrap().unwrap();
    let recorded_refusal = run_state.resident_checkpoint_refusal.unwrap();
    assert_eq!(recorded_refusal.cycle, dead_cycle);
    assert_eq!(recorded_refusal.code, "resident_record_invalid");
    assert_eq!(recorded_refusal.field.as_deref(), Some("costs.wished"));

    // Exited before finalize (step failure); next observe synthesizes incomplete.
    fx.stop_run(&dead_run, JobRunState::Failed);
    let recovery_run = fx.cycle_run(&mandate, "resident-recovery-after-invalid");
    let recovery_brief = fx.begin_cycle(&recovery_run, now + Duration::minutes(11));
    assert_eq!(recovery_brief["cycle_count"], 1, "{recovery_brief}");
    assert_eq!(recovery_brief["recovery_of"], dead_cycle);

    let dead_checkpoints = checkpoints_for_cycle(&dead_cycle);
    assert_eq!(
        dead_checkpoints.len(),
        1,
        "exactly one synthesized checkpoint"
    );
    let synthesized = &dead_checkpoints[0];
    assert_eq!(synthesized.outcome, "incomplete");
    let error = synthesized.error.as_deref().unwrap();
    assert!(
        error.contains("resident_record_invalid") && error.contains("costs.wished"),
        "synthesized error must preserve cause and field: {error}"
    );
    assert!(
        !error.contains("process killed") && !error.contains("unknown"),
        "known validation refusal must not report unknown or guessed kill: {error}"
    );
    assert_eq!(synthesized.actions.len(), 1);
    assert_eq!(synthesized.actions[0].key, action_key);
    assert_eq!(synthesized.actions[0].result, "ok");

    let replayed = fx.as_cycle_agent(&recovery_run, || {
        fx.agent_call("orbit.task.update", comment_input).unwrap()
    });
    assert_eq!(replayed["resident_action"], "already_done", "{replayed}");
    assert_eq!(replayed["key"], action_key);
    assert_eq!(
        fx.runtime.get_task_comments(&watched).unwrap().len(),
        watched_comments_after_action,
        "recovery must not repeat the completed action"
    );
    fx.write_checkpoint(
        &recovery_brief,
        &recovery_run,
        json!({
            "actions": [{
                "key": action_key,
                "kind": "orbit.task.update",
                "subject": watched,
                "result": "already_done",
                "ref": watched
            }],
            "outcome": "progress"
        }),
    );
    let finished = fx.runtime.finish_resident_cycle(&recovery_run).unwrap();
    assert_eq!(finished["synthesized"], false, "{finished}");
    assert_eq!(
        checkpoints_for_cycle(recovery_brief["cycle"].as_str().unwrap()).len(),
        1
    );
    fx.stop_run(&recovery_run, JobRunState::Success);

    // Case 2: Agent exits after `resident_record_invalid` and `resident_finalize`
    // runs directly (`finish_resident_cycle`), surfacing the cause and field.
    fx.update_task(
        &watched,
        TaskUpdateParams {
            comment: Some("Trigger finalize case".into()),
            ..Default::default()
        },
    );
    let finalize_run = fx.cycle_run(&mandate, "resident-finalize-invalid-checkpoint");
    let finalize_brief = fx.begin_cycle(&finalize_run, now + Duration::minutes(25));
    let finalize_cycle = finalize_brief["cycle"].as_str().unwrap().to_owned();
    let bad_notes_record = json!({
        "kind": "resident.checkpoint",
        "schema": 1,
        "mandate": mandate,
        "cycle": finalize_cycle,
        "run_id": finalize_run,
        "crew": "sol",
        "recovery_of": finalize_brief["recovery_of"],
        "wake": finalize_brief["wake"],
        "authority": {
            "rights": finalize_brief["authority"]["rights"],
            "observe_only": finalize_brief["authority"]["observe_only"],
            "narrowed_by": finalize_brief["authority"]["narrowed_by"]
        },
        "actions": [],
        "decisions": [],
        "requests": [],
        "costs": [],
        "notes": "x".repeat(601),
        "next_wake": {
            "reason": "heartbeat",
            "not_before": now + Duration::hours(2)
        },
        "continuity": "rebuilt",
        "outcome": "idle",
        "probe": false
    });
    let notes_refusal = call(
        &fx.runtime,
        "orbit.resident.checkpoint",
        json!({"task_id": mandate, "record": bad_notes_record}),
        McpCapability::Agent,
    )
    .unwrap_err();
    assert!(notes_refusal.contains("`notes`"), "{notes_refusal}");
    let finalize_result = fx.runtime.finish_resident_cycle(&finalize_run).unwrap();
    assert_eq!(finalize_result["synthesized"], true, "{finalize_result}");
    let finalize_checkpoints = checkpoints_for_cycle(&finalize_cycle);
    assert_eq!(finalize_checkpoints.len(), 1);
    let finalize_error = finalize_checkpoints[0].error.as_deref().unwrap();
    assert!(
        finalize_error.contains("resident_record_invalid") && finalize_error.contains("notes"),
        "finalize synthesis must preserve cause and field: {finalize_error}"
    );
    fx.stop_run(&finalize_run, JobRunState::Success);

    // Case 3: A genuinely unknown termination (process killed / silent exit with
    // no checkpoint validation refusal) stays labeled `unknown`.
    fx.update_task(
        &watched,
        TaskUpdateParams {
            comment: Some("Trigger unknown kill case".into()),
            ..Default::default()
        },
    );
    let unknown_dead_run = fx.cycle_run(&mandate, "resident-unknown-kill");
    let unknown_dead_brief = fx.begin_cycle(&unknown_dead_run, now + Duration::minutes(40));
    let unknown_dead_cycle = unknown_dead_brief["cycle"].as_str().unwrap().to_owned();
    fx.stop_run(&unknown_dead_run, JobRunState::Failed);

    let after_unknown_run = fx.cycle_run(&mandate, "resident-after-unknown-kill");
    let after_unknown_brief = fx.begin_cycle(&after_unknown_run, now + Duration::minutes(51));
    assert_eq!(after_unknown_brief["recovery_of"], unknown_dead_cycle);
    let unknown_checkpoints = checkpoints_for_cycle(&unknown_dead_cycle);
    assert_eq!(unknown_checkpoints.len(), 1);
    assert!(
        unknown_checkpoints[0]
            .error
            .as_deref()
            .is_some_and(|err| err.contains("unknown") && !err.contains("resident_record_invalid")),
        "genuinely unknown termination must remain labeled unknown: {:?}",
        unknown_checkpoints[0].error
    );
    fx.write_checkpoint(
        &after_unknown_brief,
        &after_unknown_run,
        json!({"outcome": "idle"}),
    );
    fx.finish_cycle(&after_unknown_run);

    // Case 4: If PipelineState carries no refusal diagnostic, a failed checkpoint
    // tool call recorded in `audit_events` for the run still surfaces its cause
    // and field on the synthesized incomplete checkpoint.
    fx.update_task(
        &watched,
        TaskUpdateParams {
            comment: Some("Trigger audit fallback case".into()),
            ..Default::default()
        },
    );
    let audit_dead_run = fx.cycle_run(&mandate, "resident-audit-fallback");
    let audit_dead_brief = fx.begin_cycle(&audit_dead_run, now + Duration::minutes(65));
    let audit_dead_cycle = audit_dead_brief["cycle"].as_str().unwrap().to_owned();
    fx.runtime
        .record_audit_event(&orbit_store::contract::AuditEventInsertParams {
            execution_id: format!("exec-{audit_dead_run}"),
            command: "tool".into(),
            subcommand: Some("run".into()),
            tool_name: Some("orbit.resident.checkpoint".into()),
            target_type: Some("task".into()),
            target_id: Some(mandate.clone()),
            role: "agent".into(),
            status: orbit_types::telemetry::AuditEventStatus::Failure,
            exit_code: 1,
            duration_ms: 12,
            working_directory: fx.runtime.paths().repo_root.to_string_lossy().into_owned(),
            arguments_json: None,
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: Some(
                "resident_record_invalid: invalid checkpoint field `costs.wished`: missing wished"
                    .into(),
            ),
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
            task_id: Some(mandate.clone()),
            job_run_id: Some(audit_dead_run.clone()),
            activity_id: None,
            step_index: None,
        })
        .unwrap();
    let audit_finalize = fx.runtime.finish_resident_cycle(&audit_dead_run).unwrap();
    assert_eq!(audit_finalize["synthesized"], true, "{audit_finalize}");
    let audit_checkpoints = checkpoints_for_cycle(&audit_dead_cycle);
    assert_eq!(audit_checkpoints.len(), 1);
    let audit_error = audit_checkpoints[0].error.as_deref().unwrap();
    assert!(
        audit_error.contains("resident_record_invalid") && audit_error.contains("costs.wished"),
        "audit fallback must preserve cause and field: {audit_error}"
    );
    fx.stop_run(&audit_dead_run, JobRunState::Success);
}

#[test]
fn still_live_invalid_checkpoint_correction_preserves_prior_actions_once() {
    if !crate::dispatch_admission::isolated(
        "resident_cycle::checkpoint_history::still_live_invalid_checkpoint_correction_preserves_prior_actions_once",
    ) {
        return;
    }
    let fx = fixture();
    let (watched, mandate) = fx.watched_mandate("Correct live invalid checkpoint with action");
    let run = fx.cycle_run(&mandate, "resident-live-repair-with-action");
    let brief = fx.begin_cycle(&run, Utc::now());
    let cycle = brief["cycle"].as_str().unwrap().to_owned();

    let key = fx.as_cycle_agent(&run, || {
        fx.agent_call(
            "orbit.task.update",
            json!({"id": watched, "comment": "Inspected backlog.", "model": "codex"}),
        )
        .unwrap()["resident_action_key"]
            .as_str()
            .unwrap()
            .to_owned()
    });
    let comments_after_action = fx.runtime.get_task_comments(&watched).unwrap().len();

    let mut record = json!({
        "kind": "resident.checkpoint",
        "schema": 1,
        "mandate": mandate,
        "cycle": cycle,
        "run_id": run,
        "crew": "sol",
        "recovery_of": brief["recovery_of"],
        "wake": brief["wake"],
        "authority": {
            "rights": brief["authority"]["rights"],
            "observe_only": brief["authority"]["observe_only"],
            "narrowed_by": brief["authority"]["narrowed_by"]
        },
        "actions": [{
            "key": key,
            "kind": "orbit.task.update",
            "subject": watched,
            "result": "ok",
            "ref": watched
        }],
        "decisions": [],
        "requests": [],
        "costs": [{
            "kind": "workaround",
            "what": "Checked backlog manually",
            "minutes": 1,
            "retries": 0,
            "slot_minutes": 0,
            "signature": "manual-backlog-check"
        }],
        "notes": "Retry after adding wished.",
        "next_wake": {
            "reason": "heartbeat",
            "not_before": Utc::now() + Duration::hours(2)
        },
        "continuity": "rebuilt",
        "outcome": "progress",
        "probe": false
    });

    let err = fx.as_cycle_agent(&run, || {
        call(
            &fx.runtime,
            "orbit.resident.checkpoint",
            json!({"task_id": mandate, "record": record}),
            McpCapability::Agent,
        )
        .unwrap_err()
    });
    assert!(
        err.contains("resident_record_invalid") && err.contains("costs.wished"),
        "{err}"
    );

    record["costs"][0]["wished"] = json!("Automatic backlog status summary");
    let accepted = fx.as_cycle_agent(&run, || {
        call(
            &fx.runtime,
            "orbit.resident.checkpoint",
            json!({"task_id": mandate, "record": record.clone()}),
            McpCapability::Agent,
        )
        .unwrap()
    });
    assert_eq!(accepted["checkpoint"]["actions"][0]["key"], key);
    assert_eq!(
        fx.runtime.get_task_comments(&watched).unwrap().len(),
        comments_after_action,
        "correcting the checkpoint must not repeat the earlier action"
    );
    let run_state = fx.runtime.read_run_state(&run).unwrap().unwrap();
    assert!(
        run_state.resident_checkpoint_refusal.is_none(),
        "accepted retry clears the pending refusal"
    );
    let finished = fx.runtime.finish_resident_cycle(&run).unwrap();
    assert_eq!(finished["synthesized"], false, "{finished}");
}

#[test]
fn resident_cycle_activity_aligns_with_single_accepted_checkpoint_retry_rule() {
    let activity = include_str!("../../../assets/activities/resident_cycle.yaml");
    assert!(
        activity.contains("exactly one accepted checkpoint"),
        "resident_cycle.yaml description must require one accepted checkpoint"
    );
    assert!(
        activity.contains(
            "End the cycle with exactly one accepted `orbit.resident.checkpoint` record:"
        ),
        "resident_cycle.yaml instruction must align with one accepted checkpoint"
    );
    assert!(
        !activity.contains("End the cycle with exactly one `orbit.resident.checkpoint` call:"),
        "resident_cycle.yaml must not tell the resident to make only one call before validation"
    );
    assert!(
        activity.contains("resident_record_invalid")
            && activity.contains("resident_cycle_stale")
            && activity.contains("resident_checkpoint_exists"),
        "resident_cycle.yaml must distinguish write-free validation refusals from stale or already-accepted refusals"
    );
}
