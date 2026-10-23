//! Public refusal and reconciliation controls for completion mutation provenance.
use super::*;

pub(crate) fn enabled_completion_refuses_repair_and_mutation_provenance() {
    use orbit_types::task::TaskHistoryEntry;
    use orbit_types::workflow::*;
    for (case, event) in [
        ("admitted", PR_AUTO_FIX_ADMITTED_EVENT),
        ("pending", PR_AUTO_FIX_ADMITTED_EVENT),
        ("adopted", PR_AUTO_FIX_PUSHED_EVENT),
        ("base_update", PR_BASE_UPDATE_RESERVED_EVENT),
        ("infrastructure", PR_INFRASTRUCTURE_RESERVED_EVENT),
        ("source_repair", SOURCE_REPAIR_RESERVED),
        ("linked_repair", SOURCE_REPAIR_LINKED),
        ("merge", PR_COMPLETION_MUTATION_STARTED_EVENT),
        ("auto_merge", PR_COMPLETION_MUTATION_STARTED_EVENT),
        ("malformed", PR_AUTO_FIX_ADMITTED_EVENT),
        ("local", PR_AUTO_FIX_ADMITTED_EVENT),
    ] {
        let (f, checkout) = fixture();
        let note = match case {
            "adopted" => json!(PrAutoFixPush {
                pr_number: "17".into(),
                from_head_sha: f.head.clone(),
                to_head_sha: f.head.clone(),
                blocker: PrAutoFixBlocker::FailedChecks,
                run_id: f.source.run_id.clone()
            })
            .to_string(),
            "merge" | "auto_merge" => json!({
                "run_id": f.source.run_id,
                "operation": case,
                "pr_number": "17",
                "head_sha": f.head,
            })
            .to_string(),
            "malformed" => "{".into(),
            _ => {
                // A pending repair is a later admitted attempt with no matching
                // pushed event in this history.
                let attempt = if case == "pending" { 2 } else { 1 };
                json!(PrAutoFixAttempt {
                    pr_number: "17".into(),
                    head_sha: f.head.clone(),
                    blocker: PrAutoFixBlocker::FailedChecks,
                    attempt,
                    budget: PR_AUTO_FIX_BUDGET,
                    run_id: f.source.run_id.clone(),
                    crew: "fixture".into(),
                    crew_source: "fixture".into()
                })
                .to_string()
            }
        };
        f.runtime
            .apply_task_automation_update(
                &f.task,
                TaskAutomationUpdate {
                    append_history: vec![TaskHistoryEntry {
                        at: Utc::now(),
                        by: "system".into(),
                        event: event.into(),
                        note: Some(note),
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: None,
                    }],
                    ..Default::default()
                },
            )
            .unwrap();
        if case == "local" {
            std::fs::write(checkout.join("source.txt"), "actual local repair\n").unwrap();
            git(&checkout, &["add", "source.txt"]);
            git(&checkout, &["commit", "-qm", "admitted local repair"]);
        }
        admit(&f);
        let state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
        let refusal = f
            .runtime
            .authenticate_completion_tail(&f.source.run_id, &f.task, "complete_pr")
            .unwrap_err();
        assert!(
            refusal
                .to_string()
                .contains("repair or mutation provenance"),
            "{case}: {refusal}"
        );
        assert_eq!(
            state,
            f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap(),
            "{case}: authentication never rewrites checkpoints"
        );
        tail_forge(&f, "lost_read");
        let (outcome, _) = run_tail(&f);
        assert!(
            outcome.is_err(),
            "{case}: never reuse an unchanged prefix after a mutation reservation"
        );
        if case == "local" {
            assert_ne!(git(&checkout, &["rev-parse", "HEAD"]), f.head);
            assert!(
                f.runtime
                    .get_task_artifact_manifest(&f.task)
                    .unwrap()
                    .iter()
                    .any(|a| a.path.contains("/attempts/")),
                "actual repair takes fresh governed validation before publication"
            );
        }
    }
}

pub(crate) fn expired_completion_budget_never_merges() {
    let (f, checkout) = fixture();
    let calls = tail_forge(&f, "lost_read");
    f.runtime
        .completion_deadline(&f.source.run_id, "17", 1)
        .unwrap();
    let before = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    let input = json!({"job_run_id":f.source.run_id,"completed_task_ids":[f.task],
        "workspace_path":checkout,"pr_number":"17","completion":"done","auto_fix":true,
        "head":f.branch,"published_head_sha":f.head,"base":"main","max_wait_seconds":60});
    for _ in 0..2 {
        let failure = execute_deterministic_action(
            &f.runtime,
            "pr_complete",
            &json!({}),
            &input,
            false,
            &HashMap::new(),
            None,
        )
        .unwrap_err();
        assert!(
            failure.to_string().contains("deadline expired"),
            "{failure}"
        );
    }
    let after = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    assert_eq!(
        after.completion_deadlines, before.completion_deadlines,
        "re-entry never resets the cumulative completion budget"
    );
    assert_eq!(after.step_outputs, before.step_outputs);
    assert!(
        !calls.exists(),
        "exhausted budget performs no provider read or mutation"
    );
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), f.head);
}
