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

    // Task show and the implementer's brief retain the same compact receipt.
    // Its artifact keeps advisory findings accessible without spending the
    // brief's comment budget on the full assessment.
    let shown = workspace
        .runtime
        .run_tool("orbit.task.show", json!({"id": task.id}))
        .unwrap();
    let comment = serde_json::from_value(
        shown["comments"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone(),
    )
    .unwrap();
    let audit = crate::task_pilot::pilot_audit(&workspace.runtime, &task.id, &comment);
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
    let worker_comment = serde_json::from_value(
        context["comments"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone(),
    )
    .unwrap();
    assert_eq!(comment, worker_comment);
    let worker_audit =
        crate::task_pilot::pilot_audit(&workspace.runtime, &task.id, &worker_comment);
    assert_eq!(worker_audit["assessment"], audit["assessment"]);
}

fn both_promotion_authorities_keep_real_warning_holds() {
    for sweep in [false, true] {
        let workspace = Workspace::new();
        workspace.commit_file(
            "AGENTS.md",
            "The integrator owns fixture-tool.json. Feature tasks may edit README.md only.\n",
            "record fixture ownership",
        );
        let revision = workspace.commit_file(
            "fixture-tool.json",
            r#"{"name":"fixture.read","output_schema_json":{"type":"object","properties":{"value":{"type":"string"}},"additionalProperties":false}}"#,
            "pin tool output without ready",
        );
        let missing_contract = format!(
            "Criterion 1 requires fixture.read output field ready, absent from fixture-tool.json:1 at {revision}. \
             Dependency coverage: none. AGENTS.md:1 reserves schema edits for the integrator. \
             Add an integrator prerequisite supplying ready: boolean, or rewrite the criterion."
        );
        let cases = [
            (
                "surface_warnings",
                json!({"text": "The specified surface omits a required outcome.", "holds_promotion": true}),
            ),
            (
                "utility_warnings",
                json!({"text": missing_contract, "holds_promotion": true}),
            ),
            ("surface_warnings", json!("Legacy surface warning")),
            ("utility_warnings", json!("Legacy utility warning")),
            (
                "adr_conflicts",
                json!("The current contract conflicts with this task."),
            ),
            // A prerequisite no dependency edge enforces still holds.
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
            if index == 1 {
                workspace.edit(
                    &task,
                    TaskUpdateParams {
                        description: Some("Display fixture.read readiness in README.md without changing the integrator's schema.".into()),
                        acceptance_criteria: Some(vec![
                            "Read the boolean ready output field from fixture.read.".into(),
                        ]),
                        ..Default::default()
                    },
                );
            }
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
            if index == 1 {
                let shown = workspace
                    .runtime
                    .run_tool("orbit.task.show", json!({"id": task.id}))
                    .unwrap();
                assert_eq!(
                    shown["pilot_warnings"],
                    json!([{
                        "kind": "utility_warnings", "text": missing_contract,
                    }])
                );

                // The same feature may be promoted once an explicit dependency
                // owns the missing contract. The dependency gates execution;
                // the pilot need not manufacture an additional warning hold.
                let dependency = workspace.task(
                    "Integrator adds fixture.read ready output",
                    &[],
                    &["file:fixture-tool.json"],
                    TaskComplexity::Low,
                );
                workspace.edit(
                    &dependency,
                    TaskUpdateParams {
                        description: Some(
                            "Integrator work: extend the fixture.read output schema.".into(),
                        ),
                        acceptance_criteria: Some(vec![
                            "fixture-tool.json defines ready as a boolean output field.".into(),
                        ]),
                        plan: Some(
                            "Add ready to fixture-tool.json and its output producer.".into(),
                        ),
                        ..Default::default()
                    },
                );
                workspace.edit(
                    &task,
                    TaskUpdateParams {
                        dependencies: Some(vec![dependency.id.clone()]),
                        ..Default::default()
                    },
                );
                let mut covered = assessment(&task);
                covered["assessment_rationale"] = json!(format!(
                    "fixture.read.ready is absent at {revision} (fixture-tool.json:1); \
                     dependency {} explicitly adds that boolean field within its integrator scope. \
                     The feature edits only README.md under AGENTS.md:1.",
                    dependency.id
                ));
                let covered = apply_authority(&workspace, &task, covered, sweep);
                assert_eq!(covered[key][0]["decision"], "promote", "{covered}");
                assert_eq!(workspace.status(&task), TaskStatus::Backlog);
                let shown = workspace
                    .runtime
                    .run_tool("orbit.task.show", json!({"id": task.id}))
                    .unwrap();
                assert_eq!(shown["pilot_warnings"], json!([]), "{shown}");
                assert_eq!(
                    workspace.runtime.get_task(&task.id).unwrap().dependencies(),
                    [dependency.id]
                );
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

fn dependency_warnings_agree_across_promotion_projection_and_triage() {
    for sweep in [false, true] {
        let workspace = Workspace::new();
        let dependency = workspace.task(
            "prerequisite",
            &[],
            &["file:README.md"],
            TaskComplexity::Low,
        );
        let unfinished =
            workspace.task("unrecorded", &[], &["file:README.md"], TaskComplexity::Low);
        let done = workspace
            .runtime
            .add_task(TaskAddParams {
                title: "landed prerequisite".into(),
                description: "Already landed.".into(),
                acceptance_criteria: vec!["Done.".into()],
                plan: "Nothing remains.".into(),
                status: Some(TaskStatus::Done),
                ..Default::default()
            })
            .unwrap();
        for field in ["utility_warnings", "surface_warnings"] {
            for (case, text, holds) in [
                (
                    "dependencies",
                    format!("Wait for {} to land", dependency.id),
                    false,
                ),
                (
                    "relations",
                    format!("Wait for {} to land", dependency.id),
                    false,
                ),
                ("done", format!("Wait for {} to land", done.id), false),
                (
                    "unrecorded",
                    format!("Wait for {} to land", unfinished.id),
                    true,
                ),
                (
                    "mixed",
                    format!("Wait for {} and {} to land", dependency.id, unfinished.id),
                    true,
                ),
                ("no ID", "A prerequisite has not landed".into(), true),
            ] {
                let task = workspace.task(
                    &format!("{field} {case}"),
                    &["ci-failure-sweep", "ci-failure:fixture-key"],
                    &["file:README.md"],
                    TaskComplexity::Low,
                );
                workspace.edit(
                    &task,
                    TaskUpdateParams {
                        dependencies: (case != "relations").then(|| vec![dependency.id.clone()]),
                        relations: (case == "relations").then(|| {
                            vec![TaskRelation {
                                relation_type: TaskRelationType::BlockedBy,
                                target: dependency.id.clone(),
                                regression: None,
                            }]
                        }),
                        ..Default::default()
                    },
                );
                let mut assessed = assessment(&task);
                assessed[field] = json!([{"text": text, "holds_promotion": true}]);

                // First persist without promotion authority: warning projection
                // and triage must agree while the task is still proposed.
                let pilot = workspace.running("task_pilot_pipeline", json!({}));
                let applied = workspace
                    .apply(&pilot, vec![assessed.clone()], json!({}))
                    .unwrap();
                assert_eq!(applied["status"], "succeeded", "{applied}");
                assert_eq!(workspace.status(&task), TaskStatus::Proposed);
                let shown = workspace
                    .runtime
                    .run_tool("orbit.task.show", json!({"id": task.id}))
                    .unwrap();
                assert_eq!(
                    shown["pilot_warnings"].as_array().unwrap().len(),
                    usize::from(holds),
                    "{field} {case}: {shown}"
                );
                let triage = workspace.runtime.pilot_warning_candidates_json().unwrap();
                assert_eq!(
                    triage["qualifying"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|candidate| candidate["task_id"] == task.id.as_str()),
                    holds,
                    "{field} {case}: {triage}"
                );

                let applied = apply_authority(&workspace, &task, assessed, sweep);
                let key = if sweep {
                    "ci_sweep_admission"
                } else {
                    "drain_approval"
                };
                assert_eq!(applied["status"], "succeeded", "{applied}");
                assert_eq!(
                    applied[key][0]["decision"],
                    if holds { "withhold" } else { "promote" },
                    "{field} {case}: {applied}"
                );
                if !sweep {
                    assert_eq!(applied[key][0]["approved"] == true, !holds, "{applied}");
                }
                assert_eq!(
                    workspace.status(&task),
                    if holds {
                        TaskStatus::Proposed
                    } else {
                        TaskStatus::Backlog
                    }
                );
                // Warnings never invent an edge for an unfinished named task.
                assert_eq!(
                    workspace.runtime.get_task(&task.id).unwrap().dependencies(),
                    [dependency.id.as_str()]
                );
                if !sweep {
                    let selection = workspace.select(applied["fixture_drain"].as_str().unwrap());
                    assert_eq!(
                        held_reason(&selection, &task),
                        holds.then_some("warnings"),
                        "{field} {case}: {selection}"
                    );
                }
            }
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

fn foreign_prerequisite_warning_hold_releases_after_completion() {
    let workspace = Workspace::new();
    // `review` is unfinished and is the one status that may move to `done`.
    let foreign = workspace.foreign_task("foreign prerequisite", TaskStatus::Review);
    let other_foreign =
        workspace.foreign_task("still unfinished foreign prerequisite", TaskStatus::Backlog);
    assert!(workspace.runtime.get_task(&foreign.id).is_err());
    let task = workspace.task(
        "foreign dependent",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
    );
    let missing = workspace.task(
        "missing dependent",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
    );
    let still_held = workspace.task(
        "unfinished foreign dependent",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
    );
    // No stored dependency edges: the prerequisites exist only in pilot prose.
    assert!(
        workspace
            .runtime
            .get_task(&task.id)
            .unwrap()
            .dependencies()
            .is_empty()
    );

    let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
    let selection = workspace.select(&drain);
    let pilot = workspace.pilot_child(&drain, &selection["task_ids"]);
    let held_assessment = |task: &Task, prerequisite: &str| {
        let mut assessed = assessment(task);
        assessed["blocked_by"] = json!([format!("Wait for {prerequisite} to land")]);
        assessed
    };
    let applied = workspace
        .pilot(
            &drain,
            &pilot,
            vec![
                held_assessment(&task, &foreign.id),
                held_assessment(&missing, "ORB-999999999"),
                held_assessment(&still_held, &other_foreign.id),
            ],
        )
        .unwrap();
    assert_eq!(applied["status"], "succeeded", "{applied}");
    for decision in applied["drain_approval"].as_array().unwrap() {
        assert_eq!(decision["classification"], "warnings", "{applied}");
        assert_eq!(decision["decision"], "withhold", "{applied}");
    }
    for held_task in [&task, &missing, &still_held] {
        assert_eq!(workspace.status(held_task), TaskStatus::Proposed);
        let selection = workspace.select(&drain);
        assert_eq!(
            held_reason(&selection, held_task),
            Some("warnings"),
            "{selection}"
        );
        assert!(
            !selection["task_ids"]
                .as_array()
                .unwrap()
                .contains(&json!(held_task.id)),
            "{selection}"
        );
        let shown = workspace
            .runtime
            .run_tool("orbit.task.show", json!({"id": held_task.id}))
            .unwrap();
        assert_eq!(
            shown["pilot_warnings"].as_array().unwrap().len(),
            1,
            "{shown}"
        );
        let triage = workspace.runtime.pilot_warning_candidates_json().unwrap();
        assert!(
            triage["qualifying"]
                .as_array()
                .unwrap()
                .iter()
                .any(|candidate| candidate["task_id"] == held_task.id.as_str()),
            "{triage}"
        );
    }

    // Completing the foreign prerequisite in its own workspace releases
    // only that task's hold; missing and unfinished targets still hold.
    workspace.complete_foreign_task(&foreign);
    assert_eq!(
        workspace
            .runtime
            .get_dependency_task(&foreign.id)
            .unwrap()
            .map(|done| done.status),
        Some(TaskStatus::Done)
    );
    let shown = workspace
        .runtime
        .run_tool("orbit.task.show", json!({"id": task.id}))
        .unwrap();
    assert_eq!(shown["pilot_warnings"], json!([]), "{shown}");
    let triage = workspace.runtime.pilot_warning_candidates_json().unwrap();
    assert!(
        !triage["qualifying"]
            .as_array()
            .unwrap()
            .iter()
            .any(|candidate| candidate["task_id"] == task.id.as_str()),
        "{triage}"
    );
    let selection = workspace.select(&drain);
    assert!(held_reason(&selection, &task).is_none(), "{selection}");
    assert!(
        selection["task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(task.id)),
        "released hold is selected for a fresh pilot: {selection}"
    );
    for held_task in [&missing, &still_held] {
        assert_eq!(
            held_reason(&selection, held_task),
            Some("warnings"),
            "{selection}"
        );
        let shown = workspace
            .runtime
            .run_tool("orbit.task.show", json!({"id": held_task.id}))
            .unwrap();
        assert_eq!(
            shown["pilot_warnings"].as_array().unwrap().len(),
            1,
            "{shown}"
        );
    }

    // The subsequent pilot approves without any operator edit to the task.
    let pilot = workspace.pilot_child(&drain, &selection["task_ids"]);
    let applied = workspace
        .pilot(&drain, &pilot, vec![assessment(&task)])
        .unwrap();
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(applied["drain_approval"][0]["approved"], true, "{applied}");
    assert_eq!(workspace.status(&task), TaskStatus::Backlog);
    assert_eq!(workspace.status(&missing), TaskStatus::Proposed);
    assert_eq!(workspace.status(&still_held), TaskStatus::Proposed);
}

// The public pilot/apply and drain boundary must not dispatch work before
// explicit start prerequisites have been cleared by a fresh assessment.
fn explicit_start_prerequisites_hold_until_repilot_clears_them() {
    let workspace = Workspace::new();
    let mut params = Workspace::proposed_params(
        "Wait for experiment",
        &[],
        &["file:README.md"],
        TaskComplexity::Low,
    );
    params.description =
        "Do not start before 2099-12-01T00:00:00Z and the experiment verdict.".into();
    let task = workspace.add(params);
    let rejected_drain = workspace.drain();
    let rejected_pilot = workspace.pilot_child(&rejected_drain, &json!([task.id]));
    let mut malformed = assessment(&task);
    malformed["start_prerequisites"] =
        json!([{"evidence": "Experiment verdict is unresolved", "holds_promotion": false}]);
    let rejected = workspace.pilot(&rejected_drain, &rejected_pilot, vec![malformed]);
    assert!(
        rejected.is_err() || rejected.as_ref().unwrap()["applied_count"] == 0,
        "{rejected:?}"
    );
    assert_eq!(workspace.status(&task), TaskStatus::Proposed);
    let drain = workspace.drain();
    let pilot = workspace.pilot_child(&drain, &json!([task.id]));
    let mut assessed = assessment(&task);
    assessed["start_prerequisites"] = json!([
        {"evidence": "Not before 2099-12-01T00:00:00Z"},
        {"evidence": "Experiment verdict is unresolved"}
    ]);
    assessed["utility_warnings"] =
        json!([{"text": "Implementation approach is discretionary", "holds_promotion": false}]);
    let applied = workspace.pilot_ok(&drain, &pilot, vec![assessed]);
    assert_eq!(
        applied["drain_approval"][0]["decision"], "withhold",
        "{applied}"
    );
    assert_eq!(workspace.status(&task), TaskStatus::Proposed);
    let shown = workspace
        .runtime
        .run_tool("orbit.task.show", json!({"id": task.id}))
        .unwrap();
    let warnings = shown["pilot_warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 2, "{shown}");
    assert!(
        warnings
            .iter()
            .any(|w| w["text"].as_str().unwrap().contains("2099-12-01T00:00:00Z"))
    );
    assert!(
        warnings
            .iter()
            .any(|w| w["text"].as_str().unwrap().contains("Experiment verdict"))
    );
    workspace.runtime.run_tool("orbit.task.update", json!({
        "id": task.id, "description": "The operator lifted the start requirements.", "model": "codex"
    })).unwrap();
    let current = workspace.runtime.get_task(&task.id).unwrap();
    let drain = workspace.drain();
    let pilot = workspace.pilot_child(&drain, &json!([task.id]));
    let mut assessed = assessment(&current);
    assessed["start_prerequisites"] = json!([]);
    assessed["utility_warnings"] =
        json!([{"text": "Implementation approach is discretionary", "holds_promotion": false}]);
    let applied = workspace.pilot_ok(&drain, &pilot, vec![assessed]);
    assert_eq!(
        applied["drain_approval"][0]["decision"], "promote",
        "{applied}"
    );
    assert_eq!(workspace.status(&task), TaskStatus::Backlog);
}

crate::scenario_table!(
    advisory_promotion_scenarios,
    explicit_start_prerequisites_hold_until_repilot_clears_them,
    advisory_findings_are_approved_and_reach_the_worker,
    both_promotion_authorities_keep_real_warning_holds,
    malformed_advisories_are_rejected_without_approval,
    dependency_warnings_agree_across_promotion_projection_and_triage,
    foreign_prerequisite_warning_hold_releases_after_completion
);
