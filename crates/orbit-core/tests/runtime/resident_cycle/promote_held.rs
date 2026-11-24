use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::TaskType;

use super::*;

// Apply through the same deterministic action as the pilot job so the
// resident reads a sealed, artifact-backed receipt.
fn assess(fx: &Fixture, task: &str, warnings: Value) -> String {
    let run_id = fx
        .jobs
        .insert_job_run("task_pilot_pipeline", 1, Utc::now(), None, None)
        .unwrap()
        .run_id;
    let action = |name, input| {
        fx.runtime
            .run_deterministic(name, &json!({}), &input, ToolContext::default())
            .unwrap()
    };
    let prepared = action(
        "prepare_task_pilot",
        json!({"task_ids": [task], "workspace_path": fx.repo, "base_branch": "main"}),
    );
    assert_eq!(prepared["task_ids"], json!([task]), "{prepared}");
    let revision = prepared["source"]["source_revision"].as_str().unwrap();
    let tree =
        crate::git_test_support::run(&fx.repo, &["rev-parse", &format!("{revision}^{{tree}}")])
            .unwrap();
    assert!(tree.success, "{}", tree.stderr);
    let applied = action(
        "apply_task_pilot_results",
        json!({
            "run_id": run_id,
            "workspace_path": fx.repo,
            "prepared": prepared,
            "results": [{
                "partition_index": prepared["partitions"][0]["partition_index"],
                "task_ids": [task],
                "observed_revision": revision,
                "observed_tree": tree.stdout.trim(),
                "tasks": [{
                    "task_id": task,
                    "context_files_before": prepared["tasks"][0]["context_files_before"],
                    "context_files_after": ["file:.gitignore"],
                    "disposition": "selectors",
                    "recommended_crew": "sol",
                    "recommended_complexity": "low",
                    "confidence": "high",
                    "assessment_rationale": "The ignore rule determines which scratch files can be committed.",
                    "validation_approach": "Check the matched ignore paths.",
                    "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
                    "adr_conflicts": [], "utility_warnings": warnings, "surface_warnings": [],
                    "duplicate_of": null, "already_landed": null,
                }],
            }],
        }),
    );
    assert_eq!(applied["applied_count"], 1, "{applied}");
    fx.stop_run(&run_id, JobRunState::Success);
    fx.runtime
        .get_task_comments(task)
        .unwrap()
        .last()
        .unwrap()
        .operation_id
        .clone()
        .unwrap()
}

#[test]
fn resident_backlog_update_honors_the_proposals_approval_opt_out() {
    if !crate::dispatch_admission::isolated(
        "resident_cycle::promote_held::resident_backlog_update_honors_the_proposals_approval_opt_out",
    ) {
        return;
    }
    let fx = fixture();
    let proposal = |title: &str, tags: &[&str]| {
        fx.add_task(TaskAddParams {
            title: title.into(),
            description: "Adjust the ignore rule for workspace scratch output.".into(),
            acceptance_criteria: vec!["Scratch output is ignored.".into()],
            task_type: Some(TaskType::Chore),
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            status: Some(TaskStatus::Proposed),
            ..Default::default()
        })
    };
    let clean = proposal("Clean pilot", &["no-auto-approve"]);
    let warned = proposal("Latest pilot has a warning", &["no-auto-approve"]);
    let stale = proposal("Stale pilot", &["no-auto-approve"]);
    let missing = proposal("Missing pilot", &["no-auto-approve"]);
    let design = proposal("Design-tagged proposal", &["no-auto-approve", "design"]);
    let breaking = proposal("Breaking proposal", &["no-auto-approve", "breaking-change"]);
    let no_drain = proposal("No-drain proposal", &["no-auto-approve", "no-drain"]);
    let ordinary = proposal("Ordinary proposal", &[]);
    let promote =
        |id: &str| fx.agent_call("orbit.task.update", json!({"id": id, "status": "backlog"}));

    let assessment = assess(&fx, &clean, json!([]));
    let ordinary_mandate = fx.mandate(&clean);
    fx.grant(&ordinary_mandate, &["promote", "dispatch"]);
    let ordinary_run = fx.cycle_run(&ordinary_mandate, "ordinary-promotion");
    fx.begin_cycle(&ordinary_run, Utc::now());
    fx.as_cycle_agent(&ordinary_run, || {
        let error = promote(&clean).unwrap_err();
        assert!(error.contains("needs the `promote_held` right"), "{error}");
        fx.agent_call(
            "orbit.task.update",
            json!({"id": clean, "status": "in-progress"}),
        )
        .unwrap_err();
        fx.agent_call("orbit.task.update", json!({"id": clean, "tags": []}))
            .unwrap_err();
        assert_eq!(promote(&ordinary).unwrap()["status"], "backlog");
    });

    assess(&fx, &warned, json!([]));
    assess(
        &fx,
        &warned,
        json!([{
            "text": "Adjacent cleanup is optional.",
            "holds_promotion": false,
        }]),
    );
    assess(&fx, &stale, json!([]));
    fx.update_task(
        &stale,
        TaskUpdateParams {
            description: Some("Implement a different change after the pilot.".into()),
            ..Default::default()
        },
    );
    let mandate = fx.mandate(&clean);
    fx.grant(&mandate, &["promote_held"]);
    let run = fx.cycle_run(&mandate, "held-promotion");
    let brief = fx.begin_cycle(&run, Utc::now());
    fx.as_cycle_agent(&run, || {
        for (id, reason) in [
            (&warned, "warnings"),
            (&stale, "changed_since_pilot"),
            (&missing, "current, clean task pilot"),
            (&design, "design"),
            (&breaking, "breaking-change"),
            (&no_drain, "no-drain"),
        ] {
            let error = promote(id).unwrap_err();
            assert!(error.contains(reason), "{id}: {error}");
            assert_eq!(
                fx.runtime.get_task(id).unwrap().status,
                TaskStatus::Proposed
            );
        }
        let error = promote(&proposal("Another ordinary proposal", &[])).unwrap_err();
        assert!(error.contains("needs the `promote` right"), "{error}");
        let result = promote(&clean).unwrap();
        assert_eq!(result["status"], "backlog", "{result}");
        let comments = fx.runtime.get_task_comments(&clean).unwrap();
        let audit = &comments.last().unwrap().message;
        assert!(audit.contains(&assessment), "{audit}");
        assert!(audit.contains("promote_held"), "{audit}");
        let cycle = brief["cycle"].as_str().unwrap();
        assert!(
            audit.contains(&format!("resident:{mandate}:{cycle}:")),
            "{audit}"
        );
    });
}
