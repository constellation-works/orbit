//! Advisory findings must survive admission without becoming warning holds.

use super::*;

fn advisory_findings_are_approved_and_reach_the_worker() {
    let workspace = Workspace::new();
    let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
    let task = workspace.task(
        "advisory only",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
    );
    let selection = workspace.select(&drain);
    let pilot = workspace.pilot_child(&drain, &selection["task_ids"]);
    let mut assessed = assessment(&task);
    let surface = json!({"text": "An adjacent comment uses the old section name; outside this task.", "holds_promotion": false});
    let utility =
        json!({"text": "An optional inspection command is available.", "holds_promotion": false});
    assessed["surface_warnings"] = json!([surface]);
    assessed["utility_warnings"] = json!([utility]);
    let applied = workspace.pilot(&drain, &pilot, vec![assessed]).unwrap();
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(applied["drain_approval"][0]["approved"], true, "{applied}");
    assert_eq!(workspace.status(&task), TaskStatus::Backlog);
    assert_eq!(workspace.record(&drain)["approved"], json!([task.id]));
    assert!(held_reason(&workspace.select(&drain), &task).is_none());

    // The same durable comment is returned by task show and injected in the
    // implementer's brief. Inspect its structured data, not its prose.
    let shown = workspace
        .runtime
        .run_tool("orbit.task.show", json!({"id": task.id}))
        .unwrap();
    let audit: Value = serde_json::from_str(
        shown["comments"].as_array().unwrap().last().unwrap()["message"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(audit["assessment"]["surface_warnings"], json!([surface]));
    assert_eq!(audit["assessment"]["utility_warnings"], json!([utility]));
    let context = RuntimeHost::task_context_for_agent_input(
        &workspace.runtime,
        &drain,
        "implement",
        &json!({"task_id": task.id}),
    )
    .unwrap()
    .unwrap();
    let worker_audit: Value = serde_json::from_str(
        context["comments"].as_array().unwrap().last().unwrap()["message"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(worker_audit["assessment"], audit["assessment"]);
}

fn both_promotion_authorities_keep_real_warning_holds() {
    for sweep in [false, true] {
        let workspace = Workspace::new();
        let cases = [
            (
                "surface_warnings",
                json!({"text": "The specified surface omits a required outcome.", "holds_promotion": true}),
            ),
            (
                "utility_warnings",
                json!({"text": "The lane cannot validate this criterion.", "holds_promotion": true}),
            ),
            ("surface_warnings", json!("Legacy surface warning")),
            ("utility_warnings", json!("Legacy utility warning")),
            (
                "adr_conflicts",
                json!("The current contract conflicts with this task."),
            ),
            // A prerequisite no dependency edge enforces still holds; one
            // naming a recorded dependency is covered by the new rule and
            // has its own regression check in the pilot unit tests.
            ("blocked_by", json!("Wait for ORB-999999999 to land")),
        ];
        for (index, (field, finding)) in cases.into_iter().enumerate() {
            let tags = if sweep {
                vec!["ci-failure-sweep", "ci-failure:fixture-key"]
            } else {
                vec![]
            };
            let task = workspace.task(
                &format!("hold {index}"),
                &tags,
                &["file:README.md"],
                TaskComplexity::Low,
            );
            let mut assessed = assessment(&task);
            assessed[field] = json!([finding]);
            // An advisory item must not hide a holding item in the same list.
            let advisory_field = match field {
                "surface_warnings" | "utility_warnings" => field,
                _ => "surface_warnings",
            };
            assessed[advisory_field]
                .as_array_mut()
                .unwrap()
                .push(json!({"text": "Optional adjacent cleanup.", "holds_promotion": false}));
            let applied = apply_authority(&workspace, &task, assessed, sweep);
            let key = if sweep {
                "ci_sweep_admission"
            } else {
                "drain_approval"
            };
            assert_eq!(applied["status"], "succeeded", "{applied}");
            assert_eq!(applied[key][0]["classification"], "warnings", "{applied}");
            assert_eq!(workspace.status(&task), TaskStatus::Proposed);
            if !sweep {
                let drain = applied["fixture_drain"].as_str().unwrap();
                let selection = workspace.select(drain);
                let held = selection["held"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|held| held["task_id"] == task.id.as_str())
                    .unwrap();
                assert_eq!(held["findings"].as_array().unwrap().len(), 1, "{held}");
                assert_eq!(held["findings"][0]["kind"], field);
                let text = finding
                    .as_str()
                    .unwrap_or_else(|| finding["text"].as_str().unwrap());
                assert_eq!(held["findings"][0]["text"], text);
            }
        }
        let task = workspace.task(
            "advisory sweep parity",
            &["ci-failure-sweep", "ci-failure:fixture-key"],
            &["file:README.md"],
            TaskComplexity::Low,
        );
        let mut assessed = assessment(&task);
        for field in ["surface_warnings", "utility_warnings"] {
            assessed[field] =
                json!([{"text": "Adjacent work outside scope.", "holds_promotion": false}]);
        }
        let applied = apply_authority(&workspace, &task, assessed, sweep);
        assert_eq!(applied["status"], "succeeded", "{applied}");
        assert_eq!(workspace.status(&task), TaskStatus::Backlog, "{applied}");

        // Deterministic validation findings still hold even when the agent
        // returns no holding warning of its own.
        let task = workspace.task(
            "unreachable validation",
            &["ci-failure-sweep", "ci-failure:fixture-key"],
            &["file:README.md"],
            TaskComplexity::Low,
        );
        workspace.edit(
            &task,
            TaskUpdateParams {
                acceptance_criteria: Some(vec![
                    "Validate by invoking `orbit.pipeline.invoke`.".into(),
                ]),
                ..Default::default()
            },
        );
        let mut assessed = assessment(&task);
        assessed["surface_warnings"] =
            json!([{"text": "Optional cleanup.", "holds_promotion": false}]);
        let applied = apply_authority(&workspace, &task, assessed, sweep);
        let key = if sweep {
            "ci_sweep_admission"
        } else {
            "drain_approval"
        };
        assert_eq!(applied["status"], "succeeded", "{applied}");
        assert_eq!(applied[key][0]["classification"], "warnings", "{applied}");
        assert_eq!(workspace.status(&task), TaskStatus::Proposed);
        assert!(
            !applied["tasks"][0]["validation_tool_warnings"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

fn malformed_advisories_are_rejected_without_approval() {
    let workspace = Workspace::new();
    for field in ["surface_warnings", "utility_warnings"] {
        for finding in [
            json!({"text": "Severity omitted."}),
            json!({"text": "Wrong severity type.", "holds_promotion": "false"}),
            json!({"text": " ", "holds_promotion": false}),
            json!({"holds_promotion": false}),
        ] {
            let task = workspace.task(
                "invalid advisory",
                &[],
                &["file:README.md"],
                TaskComplexity::Low,
            );
            let mut assessed = assessment(&task);
            assessed[field] = json!([finding]);
            let applied = apply_authority(&workspace, &task, assessed, false);
            assert_eq!(applied["status"], "failed", "{applied}");
            assert_eq!(
                applied["task_outcomes"][0]["outcome"], "invalid",
                "{applied}"
            );
            assert_eq!(workspace.status(&task), TaskStatus::Proposed);
            assert!(
                workspace
                    .runtime
                    .get_task_comments(&task.id)
                    .unwrap()
                    .is_empty()
            );
        }
    }
}

fn apply_authority(workspace: &Workspace, task: &Task, assessed: Value, sweep: bool) -> Value {
    if sweep {
        let run = workspace.running("ci_failure_sweep_pipeline", json!({}));
        workspace
            .apply(
                &run,
                vec![assessed],
                json!({"ci_sweep_filing": {
                    "task_id": task.id, "failure_key": "fixture-key", "tested_commit": "0123abcd",
                    "workflow": "ci", "job": "test", "step": "cargo test",
                    "run_urls": ["https://github.com/example/repo/actions/runs/1"],
                }}),
            )
            .unwrap()
    } else {
        let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
        let pilot = workspace.pilot_child(&drain, &json!([task.id]));
        let mut applied = workspace.pilot(&drain, &pilot, vec![assessed]).unwrap();
        applied["fixture_drain"] = json!(drain);
        applied
    }
}

crate::scenario_table!(
    advisory_promotion_scenarios,
    advisory_findings_are_approved_and_reach_the_worker,
    both_promotion_authorities_keep_real_warning_holds,
    malformed_advisories_are_rejected_without_approval
);
