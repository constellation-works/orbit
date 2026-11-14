//! Evidence fixtures exercise the public host handoff while task tools stay closed.

use super::*;

pub(super) fn repair_evidence_output(fixture: &Fixture) -> Value {
    let asset = load_activity_asset(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets/activities/final_recovery.yaml"),
        )
        .unwrap(),
    )
    .unwrap();
    let orbit_types::workflow::ActivityV2Spec::AgentLoop(spec) = asset.spec.spec else {
        unreachable!()
    };
    let policy = orbit_types::workflow::ActivityToolDenyPolicy {
        activity: "final_recovery".into(),
        disallow_list: spec.tool_disallow_list.unwrap(),
    };
    let other = fixture
        .runtime
        .add_task(orbit_core::application::task::TaskAddParams {
            title: "Another task".into(),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    for task in [&fixture.task_id, &other] {
        for tool in ["orbit.task.update", "orbit.task.artifact.put"] {
            let error = RuntimeHost::run_tool_with_context(
                &fixture.runtime,
                tool,
                json!({"id": task, "model": "codex"}),
                ToolContext {
                    tool_deny_policy: Some(policy.clone()),
                    ..Default::default()
                },
            )
            .unwrap_err();
            assert!(error.to_string().contains("disallow"), "{tool}: {error}");
        }
    }
    let scratch = fixture.repo.join(".orbit/tmp");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join("repair.log"), "repair validation passed\n").unwrap();
    let evidence = json!({
        "execution_summary": "Outcome: success\nChanges: repaired candidate with evidence.",
        "task_artifacts": [{"path": "recovery/repair.log", "source_path": ".orbit/tmp/repair.log"}],
    });
    let run = fixture.input["job_run_id"].as_str().unwrap();
    let persist = |task: &str, value: &Value| {
        RuntimeHost::persist_final_recovery_evidence(
            &fixture.runtime,
            run,
            task,
            &fixture.repo,
            value,
        )
    };
    let error = persist(&other, &evidence).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no undecided final recovery for this task"),
        "{error}"
    );
    assert!(
        fixture
            .runtime
            .get_task_artifacts(&other)
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .runtime
            .get_task(&other)
            .unwrap()
            .execution_summary
            .is_empty()
    );

    let original_summary = fixture
        .runtime
        .get_task(&fixture.task_id)
        .unwrap()
        .execution_summary;
    let mut invalid = evidence.clone();
    invalid["task_id"] = json!(other);
    assert!(
        persist(&fixture.task_id, &invalid).is_err(),
        "no caller-selected target"
    );
    for bad in [
        json!({"path": "review-report.json", "source_path": ".orbit/tmp/repair.log"}),
        json!({"path": "recovery/outside.log", "source_path": "candidate.txt"}),
        json!({"path": "recovery/absent.log", "source_path": ".orbit/tmp/absent.log"}),
        evidence["task_artifacts"][0].clone(),
    ] {
        let mut invalid = evidence.clone();
        invalid["task_artifacts"].as_array_mut().unwrap().push(bad);
        assert!(
            persist(&fixture.task_id, &invalid).is_err(),
            "the entire batch must be valid"
        );
        assert!(
            fixture
                .runtime
                .get_task_artifact(&fixture.task_id, "recovery/repair.log")
                .unwrap()
                .is_none(),
            "an invalid later entry cannot partially attach the earlier one"
        );
        assert_eq!(
            fixture
                .runtime
                .get_task(&fixture.task_id)
                .unwrap()
                .execution_summary,
            original_summary
        );
    }
    json!({"decision": "resume", "fault": "implementer", "fault_evidence": "run fixture: the candidate failed its check", "step_id": "review_gate_settle",
        "rationale": "repaired and verified candidate", "repair_evidence": evidence})
}
