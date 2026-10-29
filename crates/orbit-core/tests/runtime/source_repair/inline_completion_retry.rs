//! Adversarial public admission checks for the supported completion retry history.
use super::inline_publication::{refused_without_effects, rework, supported_incident};
use super::published_owner::save_run;
use super::*;
use orbit_types::workflow::JobRun;

pub(super) fn audit_started(f: &Fixture, run: &JobRun, step: &str) {
    use orbit_types::workflow::activity_job::{V2AuditEnvelope, V2AuditEvent, V2AuditEventKind};
    let kind = V2AuditEventKind::StepStarted {
        step_id: step.into(),
    };
    let event = V2AuditEvent {
        envelope: V2AuditEnvelope {
            schema_version: 1,
            event_type: kind.event_type().into(),
            event_id: format!("{}-{step}-started", run.run_id),
            ts: run.created_at,
            run_id: run.run_id.clone(),
            agent_identity: "system".into(),
            parent_event_id: None,
            workspace_path: None,
        },
        kind,
    };
    f.runtime
        .insert_v2_audit_event(&orbit_core::V2AuditEventInsertParams {
            workspace_id: f.runtime.workspace_id().unwrap(),
            event_id: event.envelope.event_id.clone(),
            source: "v2_envelope".into(),
            schema_version: 1,
            event_type: event.envelope.event_type.clone(),
            ts: event.envelope.ts,
            run_id: run.run_id.clone(),
            agent_identity: "system".into(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: serde_json::to_string(&event).unwrap(),
        })
        .unwrap();
}

fn supported_retry_refuses_unbound_grants_receipts_and_checkpoints() {
    let (f, retry, held) = supported_incident();
    let stale = rework(&f);
    for member in [&retry, &held] {
        for key in [
            "task_id",
            "source_run_id",
            "pr_number",
            "head_sha",
            "drain_run_id",
            "blocker",
        ] {
            let mut changed = member.clone();
            changed.input.as_mut().unwrap()["out_of_run_auto_fix"][key] = json!("foreign");
            save_run(&f, &changed);
            refused_without_effects(&f, f.request("inspect"), key);
            refused_without_effects(&f, stale.clone(), key);
            save_run(&f, member);
        }
        for case in [
            "missing grant",
            "null grant",
            "incomplete parent",
            "cyclic parent",
            "ambiguous ordering",
        ] {
            let mut changed = member.clone();
            match case {
                "missing grant" => {
                    changed
                        .input
                        .as_mut()
                        .unwrap()
                        .as_object_mut()
                        .unwrap()
                        .remove("out_of_run_auto_fix");
                }
                "null grant" => {
                    changed.input.as_mut().unwrap()["out_of_run_auto_fix"] = Value::Null
                }
                "incomplete parent" => changed.retry_source_run_id = Some("missing".into()),
                "cyclic parent" => changed.retry_source_run_id = Some(held.run_id.clone()),
                "ambiguous ordering" => changed.created_at = f.source.created_at,
                _ => unreachable!(),
            }
            save_run(&f, &changed);
            refused_without_effects(&f, f.request("inspect"), case);
            save_run(&f, member);
        }
        let original = f.runtime.read_run_state(&member.run_id).unwrap().unwrap();
        for case in [
            "unrelated deadline",
            "old unrenewed deadline",
            "changed push",
            "extra source output",
            "final recovery",
            "rebase recovery",
        ] {
            let mut changed = original.clone();
            match case {
                "unrelated deadline" => {
                    changed.completion_deadlines.insert("18".into(), u64::MAX);
                }
                "old unrenewed deadline" => {
                    changed.completion_deadlines.insert("17".into(), 1);
                }
                "changed push" => changed.pipeline["push"]["local_sha"] = json!(f.base),
                // Outputs without a corresponding step-state entry must also refuse.
                "extra source output" => {
                    changed.step_outputs.insert(99, json!({"head":f.base}));
                }
                "final recovery" => changed.final_recovery = Some(serde_json::from_value(json!({"key":{"run_id":member.run_id,"attempt":1}, "task_id":f.task,"failed_step_id":"complete_pr","admitted_at":Utc::now()})).unwrap()),
                "rebase recovery" => {
                    changed
                        .rebase_recovery_checkpoints
                        .insert("complete_pr".into(), json!({"head":f.base}));
                }
                _ => unreachable!(),
            }
            assert_ne!(changed, original, "{case} must change the host proof");
            f.runtime.write_run_state(&member.run_id, &changed).unwrap();
            refused_without_effects(&f, f.request("inspect"), case);
            f.runtime
                .write_run_state(&member.run_id, &original)
                .unwrap();
        }
        for key in [
            "handoff_run_id",
            "task_id",
            "task_spec_digest",
            "pr_number",
            "pr_url",
            "failed_step_id",
            "candidate_preserved",
        ] {
            let mut changed = original.clone();
            changed.failure_activity_checkpoint.as_mut().unwrap().output[key] = json!("foreign");
            f.runtime.write_run_state(&member.run_id, &changed).unwrap();
            refused_without_effects(&f, f.request("inspect"), key);
            f.runtime
                .write_run_state(&member.run_id, &original)
                .unwrap();
        }
        // A changed host record must invalidate the previously inspected binding
        // even when it still proves that this retry had no source effects.
        let mut changed = member.clone();
        changed.duration_ms = match member.duration_ms {
            Some(_) => None,
            None => Some(42),
        };
        assert_ne!(
            changed, *member,
            "host proof mutation must change the record"
        );
        save_run(&f, &changed);
        let error = refused_without_effects(&f, stale.clone(), "stale inspected lineage");
        assert!(error.contains("binding"), "{error}");
        save_run(&f, member);
    }
    f.inspect();
    let checkout = PathBuf::from(
        f.state.pipeline["worktree"]["workspace_path"]
            .as_str()
            .unwrap(),
    );
    let dirty = checkout.join("unfinished-repair.txt");
    std::fs::write(&dirty, "unfinished repair").unwrap();
    refused_without_effects(&f, f.request("inspect"), "dirty shared candidate");
    std::fs::remove_file(dirty).unwrap();
    git(
        &checkout,
        &[
            "commit",
            "--allow-empty",
            "-qm",
            "divergent unpublished head",
        ],
    );
    refused_without_effects(&f, f.request("inspect"), "divergent shared candidate");
}

fn supported_retry_refuses_source_audits_and_mutation_history() {
    for case in [
        "source audit",
        "pr_auto_fix_admitted",
        "pr_auto_fix_pushed",
        "pr_completion_mutation_started",
    ] {
        let (f, retry, _) = supported_incident();
        let request = rework(&f);
        if case == "source audit" {
            audit_started(&f, &retry, "implement");
        } else {
            f.runtime
                .apply_task_automation_update(
                    &f.task,
                    TaskAutomationUpdate {
                        append_history: vec![orbit_types::task::TaskHistoryEntry {
                            at: retry.created_at,
                            by: "system".into(),
                            event: case.into(),
                            // Even an incomplete reservation cannot prove no agent effects.
                            note: Some("{}".into()),
                            from_status: None,
                            to_status: None,
                            operation_id: None,
                            decision: None,
                        }],
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        refused_without_effects(&f, f.request("inspect"), case);
        refused_without_effects(&f, request, case);
    }
}

fn supported_retry_requires_unique_complete_host_grant_history() {
    let (f, _, held) = supported_incident();
    let events = f
        .runtime
        .global_root()
        .join("tasks/workspaces")
        .join(f.runtime.workspace_id().unwrap())
        .join(&f.task)
        .join(orbit_types::task::TASK_EVENTS_FILE_NAME);
    let original = std::fs::read_to_string(&events).unwrap();
    let rows = original
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let index = rows
        .iter()
        .position(|row| row["type"] == "pr_auto_fix_out_of_run")
        .unwrap();
    for case in [
        "missing grant",
        "duplicate grant",
        "malformed grant",
        "foreign author",
        "foreign drain",
        "foreign source",
        "foreign origin",
    ] {
        // Corrupt only this fixture's host store to verify each authentication
        // term independently. Production admission never rewrites history.
        let mut changed = rows.clone();
        match case {
            "missing grant" => {
                changed.remove(index);
            }
            "duplicate grant" => changed.push(changed[index].clone()),
            "malformed grant" => changed[index]["note"] = json!("{"),
            "foreign author" => changed[index]["by"] = json!("agent"),
            _ => {
                let mut note: Value =
                    serde_json::from_str(changed[index]["note"].as_str().unwrap()).unwrap();
                let key = match case {
                    "foreign drain" => "drain_run_id",
                    "foreign source" => "source_run_id",
                    _ => "resumed_run_id",
                };
                note[key] = json!("foreign");
                changed[index]["note"] = json!(note.to_string());
            }
        }
        let raw = changed
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&events, raw).unwrap();
        refused_without_effects(&f, f.request("inspect"), case);
        std::fs::write(&events, &original).unwrap();
    }
    f.inspect();
    f.runtime
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "DELETE FROM v2_audit_events WHERE run_id = ?1",
                    [&held.run_id],
                )
                .unwrap();
            Ok(())
        })
        .unwrap();
    let error = refused_without_effects(&f, f.request("inspect"), "incomplete audit");
    assert!(error.contains("audit evidence missing"), "{error}");
}

crate::scenario_table!(
    inline_completion_retry_scenarios,
    supported_retry_refuses_unbound_grants_receipts_and_checkpoints,
    supported_retry_refuses_source_audits_and_mutation_history,
    supported_retry_requires_unique_complete_host_grant_history
);
