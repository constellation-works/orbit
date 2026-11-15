use super::*;

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
                orbit_store::contracts::TaskHistoryUpdateParams {
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
